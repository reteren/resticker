# R3: Win32-механика тайлинга — Что реально работает

Исследование низкоуровневых механизмов Windows Win32 API и DWM для реализации полноценного тайлинг-оконного менеджера (Tiling Window Manager) в архитектуре `resticker`.

---

## 1. Фильтр управляемости (Window Manageability Filter)

Тайлинг-менеджер обязан детерминированно разделять окна на три категории: **Tiled** (встраиваемые в сетку раскладки), **Floating** (плавающие поверх сетки: диалоги, утилиты) и **Ignored** (системные окна, оверлеи, фоновые процессы).

### 1.1 Точный алгоритм фильтрации

1. **`IsWindowVisible(hwnd)` [ИСТОЧНИК: MSDN IsWindowVisible]**:
   - Окно обязано иметь стиль `WS_VISIBLE`. Пропускает скрытые окна-слушатели сообщений (`HWND_MESSAGE`), неинициализированные дескрипторы.
   - *Нюанс:* `IsWindowVisible == TRUE` не гарантирует отображения на экране (окно может иметь размер 0×0 или быть замаскировано DWM).
2. **Стиль `WS_EX_TOOLWINDOW` и переопределение `WS_EX_APPWINDOW` [ИСТОЧНИК: MSDN Extended Window Styles]**:
   - `WS_EX_TOOLWINDOW` (плавающие палитры, тултипы) исключаются из тайлинга и Alt+Tab.
   - Если выставлен `WS_EX_APPWINDOW`, окно принудительно выводится на панель задач и должно считаться полноценным окном, даже при наличии `WS_EX_TOOLWINDOW`.
3. **Стиль `WS_EX_NOACTIVATE` [ИСТОЧНИК: MSDN Extended Window Styles]**:
   - Окна, не принимающие фокус ввода (HUD, клик-сквозные оверлеи вроде самого оверлея `resticker`). Попытка тайлить такое окно ломает стек фокуса системы. Категория: *Ignored*.
4. **Дочерние (`WS_CHILD`) и принадлежащие (`GW_OWNER`) окна [ИСТОЧНИК: MSDN GetWindow, GW_OWNER]**:
   - Дочерние окна (`GetAncestor(hwnd, GA_ROOT) != hwnd` или `GetWindowLongW(hwnd, GWL_STYLE) & WS_CHILD != 0`) исключаются.
   - Окна с владельцем (`GetWindow(hwnd, GW_OWNER) != NULL`): выпадающие списки, модальные диалоги, комбобоксы, палитры. Если у окна есть владелец и отсутствует `WS_EX_APPWINDOW`, его нельзя встраивать в дерево тайлинга как корневой узел — оно должно переходить в категорию *Floating*.
5. **DWM Cloaked Windows (`DWMWA_CLOAKED`) [ИСТОЧНИК: MSDN DwmGetWindowAttribute, DWMWINDOWATTRIBUTE]**:
   - DWM скрывает рендеринг окна, сохраняя `IsWindowVisible == TRUE`. `DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &flags, sizeof(flags))` возвращает битовую маску:
     - `DWM_CLOAKED_APP (0x00000001)`: Окно скрыто самим приложением (UWP/XAML в фоне, приостановленный процесс).
     - `DWM_CLOAKED_SHELL (0x00000002)`: Окно скрыто оболочкой Windows — окно находится на **другом виртуальном рабочем столе (Virtual Desktop)** или скрыто Task View.
     - `DWM_CLOAKED_INHERITED (0x00000004)`: Состояние унаследовано от окна-владельца.
   - *Для тайлинга:* Окна с `DWM_CLOAKED_SHELL` должны привязываться к своим виртуальным воркспейсам и не участвовать в раскладке текущего экрана.
6. **UWP / `ApplicationFrameWindow` и `Windows.UI.Core.CoreWindow` [ИСТОЧНИК: Chromium/Electron Window Enumeration Internals]**:
   - Современные приложения Windows (Settings, Calculator, Terminal) работают внутри хост-окна класса `ApplicationFrameWindow`.
   - Внутреннее содержимое живет в `Windows.UI.Core.CoreWindow`.
   - *Правило:* Тайлится верхний `ApplicationFrameWindow`, но только если `DwmGetWindowAttribute(DWMWA_CLOAKED)` не возвращает `DWM_CLOAKED_APP`, и дочерний `CoreWindow` уже инициализирован (проверяется через `FindWindowExW(hwnd, None, w!("Windows.UI.Core.CoreWindow"), None)`).
7. **Окна без заголовка / Splash-экраны (`WS_POPUP` без `WS_THICKFRAME`) [ИСТОЧНИК: Komorebi / GlazeWM architecture]**:
   - Всплывающие splash-экраны (Photoshop, IDE, лаунчеры игр) создаются как `WS_POPUP`.
   - Если окно имеет `WS_POPUP` и не имеет `WS_THICKFRAME` / `WS_CAPTION`, оно исключается из сетки (*Floating* или *Ignored*).
8. **Диалоги и модальные окна (`#32770`, `DS_MODALFRAME`) [ИСТОЧНИК: MSDN Dialog Boxes]**:
   - Системный класс `#32770` или окна с `GetWindow(hwnd, GW_ENABLEDPOPUP) != hwnd` блокируют поток владельца и имеют жестко сверстанные контролы. Попытка ресайза ломает верстку диалога. Категория: *Floating*.
9. **Системные окна шелла (`Progman`, `WorkerW`, `Shell_TrayWnd`) [ИСТОЧНИК: MSDN Shell Windows]**:
   - Окна рабочего стола (`Progman`, `WorkerW`) и панели задач (`Shell_TrayWnd`, `Shell_SecondaryTrayWnd`) жестко отфильтровываются по имени класса и через `GetShellWindow()`.
10. **Окна с фиксированным размером (`WS_THICKFRAME`, `WS_MAXIMIZEBOX`) [ИСТОЧНИК: MSDN Window Styles]**:
    - Окно без стиля `WS_THICKFRAME` (изменение размера) и без `WS_MAXIMIZEBOX` не поддерживает масштабирование (например, классические утилиты, инсталляторы). Попытка встроить их в тайл приведет к искажению или отказу ресайза. Категория: *Floating*.
11. **Ограничения `WM_GETMINMAXINFO` (`MINMAXINFO.ptMinTrackSize`) [ИСТОЧНИК: MSDN WM_GETMINMAXINFO]**:
    - Если тайлинг-слот меньше, чем минимальный размер `ptMinTrackSize`, `SetWindowPos` физически не сможет ужать окно (система ограничит размер), и окно вылезет за пределы своего тайла, перекрыв соседей.

### 1.2 Сравнение с существующим фильтром `rst-win32::window_enum`

Текущий фильтр `is_real_window` в [crates/rst-win32/src/window_enum.rs:151-171](file:///C:/resticker/crates/rst-win32/src/window_enum.rs#L151-L171) проектировался под задачу M4 (оклюдеры маски видимости):
- **Что уже есть:** проверка `visible`, `!cloaked` ([window_enum.rs:266](file:///C:/resticker/crates/rst-win32/src/window_enum.rs#L266)), `is_root` (`GA_ROOT == hwnd`), `!no_activate`, исключение `tool_window` и `has_owner` с оглядкой на `app_window`.
- **Где фильтр слабее нужного для тайлинга:**
  1. `cloaked` схлопывается в булев флаг `cloaked != 0` ([window_enum.rs:266](file:///C:/resticker/crates/rst-win32/src/window_enum.rs#L266)) без разделения `DWM_CLOAKED_SHELL` (виртуальные столы) и `DWM_CLOAKED_APP`.
  2. Не проверяются флаги масштабируемости (`WS_THICKFRAME`, `WS_MAXIMIZEBOX`): для маски оклюзии фиксированные окна подходят, для тайлинга их ресайзить нельзя.
  3. Классы шелла (`Progman`, `WorkerW`, `Shell_TrayWnd`) фильтруются только в контексте `shell_switching()` ([window_enum.rs:289-298](file:///C:/resticker/crates/rst-win32/src/window_enum.rs#L289-L298)), но не исключаются из `is_real_window`.
  4. Нет детекции диалогов (`#32770`, `DS_MODALFRAME`) и разделения на слои *Tiled* vs *Floating*.
  5. Не запрашивается `MINMAXINFO` для валидации минимальных габаритов окна перед встраиванием в сплит.

---

## 2. Геометрия, гэпы и DPI (Geometry, Gaps & DPI)

### 2.1 GetWindowRect vs DWMWA_EXTENDED_FRAME_BOUNDS

Начиная с Windows 10 (тема DWM Aero), стандартный `GetWindowRect` возвращает габариты окна вместе с **невидимыми границами захвата мыши (resize borders)** и прозрачной тенью (~7–8 px по бокам и снизу, ~0–1 px сверху) [ИЗМЕРЕНО на Windows 10/11].

- `GetWindowRect(hwnd)`: логический прямоугольник Win32 с невидимыми полями.
- `DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS)`: фактический видимый прямоугольник, отрисовываемый DWM композитором.
- `SetWindowPos(hwnd, ...)`: принимает координаты в пространстве `GetWindowRect`.

Если передать визуальный прямоугольник в `SetWindowPos` напрямую, невидимые рамки приведут к взаимному наложению окон и искажению расчетных гэпов на 14–16 px.

### 2.2 Формула расчета целевого прямоугольника (как решено в проекте)

В проекте точная компенсация невидимых полей реализована в `set_dwm_bounds` ([crates/rst-win32/src/window_pin.rs:701-753](file:///C:/resticker/crates/rst-win32/src/window_pin.rs#L701-L753)):

```rust
// crates/rst-win32/src/window_pin.rs:712-716
let gwr = GetWindowRect(hwnd);
let dwm = extended_frame_bounds(hwnd); // DWMWA_EXTENDED_FRAME_BOUNDS
let dx = dwm.x - gwr.left; // смещение левого края (~ -7 px)
let dy = dwm.y - gwr.top;  // смещение верхнего края (~ 0 px)
let dw = dwm.w - (gwr.right - gwr.left); // дельта ширины (~ -14 px)
let dh = dwm.h - (gwr.bottom - gwr.top); // дельта высоты (~ -8 px)

// Для целевого видимого слота тайла `target` с гэпом G:
SetWindowPos(
    hwnd, None,
    target.left - dx,
    target.top - dy,
    target.width - dw,
    target.height - dh,
    flags
);
```

Эта формула обеспечивает идеальный визуальный зазор ровно в $G$ пикселей между видимыми границами окон.

### 2.3 Проблема Per-Monitor DPI

1. **Разнородный DPI:** При переносе окна с монитора 100% (96 DPI) на 150% (144 DPI) Windows шлет окну `WM_DPICHANGED` [ИСТОЧНИК: MSDN WM_DPICHANGED]. Размеры невидимых полей масштабируются пропорционально (при 150% невидимая рамка вырастает с 7 px до ~11 px) [ИЗМЕРЕНО].
2. **Осведомленность resticker:**
   - В манифесте [crates/resticker/resources/resticker.exe.manifest:6](file:///C:/resticker/crates/resticker/resources/resticker.exe.manifest#L6) объявлено:
     `<dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>`
   - `PerMonitorV2` гарантирует, что координаты в вызовах Win32 API (`GetWindowRect`, `SetWindowPos`, `EnumDisplayMonitors`) не подвергаются битмап-масштабированию и соответствуют физическим пикселям виртуального десктопа.
   - При перемещении чужих окон между мониторами пересчет дельт $(dx, dy, dw, dh)$ должен выполняться **после** смены монитора окна, когда целевое окно обновило свои DPI-метрики.

---

## 3. Массовое перемещение и подавление мерцания (Batch Positioning)

### 3.1 BeginDeferWindowPos / DeferWindowPos / EndDeferWindowPos

- **Документация [ИСТОЧНИК: MSDN DeferWindowPos]:** «All windows in the structure must have the same parent». Для top-level окон родителем выступает Desktop (`GetDesktopWindow()`), поэтому пакетная перестановка 5–10 окон верхнего уровня через `BeginDeferWindowPos` / `EndDeferWindowPos` полностью валидна.
- **Реальный эффект на мерцание:**
  - `EndDeferWindowPos` обновляет позиции всех окон в ядре Win32k и DWM за один атомарный системный переход. Это предотвращает промежуточный Z-order джиттер и взаимное перекрытие рамок.
  - **Ограничение [ИСТОЧНИК: Microsoft Windows Internals]:** Каждое чужое приложение обрабатывает `WM_WINDOWPOSCHANGING` / `WM_SIZE` в **своем собственном UI-потоке** и перерисовывает swapchain (DirectX/Vulkan/GDI) асинхронно. `DeferWindowPos` устраняет рассинхрон рамок DWM, но не может принудить 10 независимых процессов отрисовать новые буферы кадров в один и тот же момент времени.

### 3.2 Флаги позиционирования (`SetWindowPos` / `DeferWindowPos`)

| Флаг | Значение и польза для тайлинга | Риски и подводные камни |
|---|---|---|
| `SWP_ASYNCWINDOWPOS` `(0x4000)` | Отправляет запрос в очередь потока окна, не блокируя поток тайлинг-менеджера [ИСТОЧНИК: MSDN SetWindowPos]. Критично при наличии зависших окон. | Нельзя синхронно прочитать новый `GetWindowRect` сразу после вызова (требуется ждать `EVENT_OBJECT_LOCATIONCHANGE`). |
| `SWP_NOSENDCHANGING` `(0x0400)` | Не отправляет окну `WM_WINDOWPOSCHANGING`. Ускоряет позиционирование, предотвращает вмешательство окна в расчетные размеры. | Окна с кастомным лейаутом (WPF/Qt/Electron), рассчитывающие клиентскую область на этом сообщении, могут выдать артефакт в первом кадре. |
| `SWP_NOCOPYBITS` `(0x0100)` | Запрещает DWM сохранять и растягивать старый битмап клиентской области при ресайзе. | Исключает "растянутый мыльный кадр", но при медленной перерисовке окна возможна кратковременная вспышка фонового цвета. |
| `SWP_NOREDRAW` `(0x0008)` | Полностью подавляет перерисовку клиентской области и неклиентской рамки. | Окно замирает в старом виде до явного вызова `RedrawWindow` или `InvalidateRect`. |

### 3.3 Подавление системных анимаций DWM

Windows по умолчанию применяет ~250 мс анимации изменения размера, восстановления и минимизации окон. В тайлинге это выглядит как медлительность и "желейность" перекладки.

- В проекте подавление анимаций реализовано через `DWMWA_TRANSITIONS_FORCEDISABLED` ([crates/rst-win32/src/window_pin.rs:76, 230-243](file:///C:/resticker/crates/rst-win32/src/window_pin.rs#L76)):
  ```rust
  // crates/rst-win32/src/window_pin.rs:235-241
  DwmSetWindowAttribute(
      hwnd,
      DWMWA_TRANSITIONS_FORCEDISABLED,
      (&raw const value).cast(),
      size_of::<BOOL>() as u32,
  );
  ```
  Это отключает интерполяцию DWM для конкретного окна, позволяя мгновенно перестраивать тайловую сетку без визуальных задержек.

---

## 4. Состояния окна: Maximized, Minimized, Normal, Restoring

### 4.1 Корректный вывод из Maximized перед тайлингом

- **Подвох Win32 [ИЗМЕРЕНО в window_pin.rs]:** Если окно находится в состоянии `WS_MAXIMIZE` (`IsZoomed(hwnd) == TRUE`), вызов `SetWindowPos(hwnd, ..., w, h, ...)` молча игнорируется оконным менеджером Windows — окно остается развернутым на весь монитор.
- **Проблема `ShowWindow(SW_RESTORE)`:**
  1. Восстанавливает окно в его старые пре-максимизированные координаты (лишний кадр и визуальный прыжок).
  2. Активирует окно и крадет фокус ввода.
- **Атомарное решение через `SetWindowPlacement`:**
  В проекте это решено в [crates/rst-win32/src/window_pin.rs:431-468, 720-736](file:///C:/resticker/crates/rst-win32/src/window_pin.rs#L431-L468):
  ```rust
  // crates/rst-win32/src/window_pin.rs:453-467
  let placement = WINDOWPLACEMENT {
      length: size_of::<WINDOWPLACEMENT>() as u32,
      showCmd: SW_SHOWNOACTIVATE.0 as u32, // без активации и кражи фокуса!
      rcNormalPosition: RECT {
          left: x, top: y, right: x + w, bottom: y + h,
      },
      ..Default::default()
  };
  unsafe { SetWindowPlacement(target_hwnd, &placement) };
  ```
  `SetWindowPlacement` одним вызовом сбрасывает `WS_MAXIMIZE` и сразу позиционирует окно в координаты тайла `rcNormalPosition` без промежуточного кадра и без кражи фокуса.

### 4.2 Перехват действий пользователя через WinEvent-хуки

В [crates/rst-win32/src/window_tracker.rs:327-348, 554-567](file:///C:/resticker/crates/rst-win32/src/window_tracker.rs#L327-L348) уже реализован диспетчер событий:
- `EVENT_SYSTEM_MINIMIZESTART` `(0x0016)`: пользователь свернул окно. Тайлинг-менеджер удаляет узел из активного дерева раскладки и перераспределяет освободившееся пространство между соседями.
- `EVENT_SYSTEM_MINIMIZEEND` `(0x0017)`: окно восстановлено. Тайлинг-менеджер заново вставляет окно в активный тайл и применяет `set_dwm_bounds`.
- `EVENT_SYSTEM_MOVESIZESTART` / `MOVESIZEEND` `(0x000A / 0x000B)`: пользователь начал/закончил ручное перетаскивание или ресайз окна.
- `EVENT_OBJECT_LOCATIONCHANGE` `(0x800B)`: изменение координат (дебаунсится 16 мс в [window_tracker.rs:46](file:///C:/resticker/crates/rst-win32/src/window_tracker.rs#L46)).

---

## 5. Конфликты с подсистемами Windows (Snap Layouts, Aero Snap)

### 5.1 Механика конфликтов в Windows 10/11

1. **Aero Snap / Snap Layouts:** При перетаскивании окна к краям экрана шелл перехватывает управление и разворачивает окно на 50%/25% экрана, вызывая оверлей Snap Assist.
2. **Системные хоткеи:** `Win + Left/Right/Up/Down` зарезервированы проводником (`explorer.exe`) для системного снапа.

### 5.2 Программное отключение через реестр и почему это неприемлемо

Ключи реестра:
- `HKCU\Control Panel\Desktop\WindowArrangementActive` ("0" / "1") [ИСТОЧНИК: MSDN SystemParametersInfo SPI_SETWINARRANGEMENTACTIVE]
- `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced\EnableSnapAssist` (0 / 1)

*Почему нельзя отключать глобально:*
- Требует перезапуска Explorer или глобального `SystemParametersInfo(SPI_SETWINARRANGEMENTACTIVE, ...)`.
- Меняет поведение системы глобально для всех игр и приложений пользователя.
- При аварийном завершении resticker пользователь останется с поломанным стандартным поведением Windows.

### 5.3 Безопасные альтернативы перехвата

1. **Перехват хоткеев `Win + Стрелки`:** Регистрация глобальных хоткеев через `RegisterHotKey` (или низкоуровневый `WH_KEYBOARD_LL` хук) — когда resticker регистрирует эти комбинации, шелл Windows их больше не получает.
2. **Перехват перетаскивания (Move-lock / Snap-back):**
   - В [crates/rst-win32/src/window_pin.rs:627-657](file:///C:/resticker/crates/rst-win32/src/window_pin.rs#L627-L657) на `EVENT_SYSTEM_MOVESIZESTART` отправляется `PostMessageW(hwnd, WM_CANCELMODE, ...)` для обрыва модального цикла перетаскивания.
   - Реактивный snap-back на `EVENT_SYSTEM_MOVESIZEEND`: когда пользователь отпускает окно, тайлинг-менеджер мгновенно возвращает его в расчетный тайл через `set_dwm_bounds`.

---

## 6. Управление фокусом (Focus Management & SetForegroundWindow)

### 6.1 Системное ограничение Foreground Activation Lock

[ИСТОЧНИК: MSDN SetForegroundWindow, Foreground Activation Restrictions]:
Windows запрещает фоновым процессам произвольно перехватывать передний план. Вызов `SetForegroundWindow(hwnd)` фоновым процессом приводит лишь к миганию кнопки окна на панели задач (Taskbar Flashing), но окно не активируется.

Разрешение на передачу фокуса дается только если:
1. Вызывающий процесс сам является текущим foreground-процессом.
2. Вызывающий процесс получил последнее аппаратное событие ввода (мышь/клавиатура).
3. Процесс переднего плана явно вызвал `AllowSetForegroundWindow(pid)`.

### 6.2 Обходные пути и их риски

1. **`AttachThreadInput` [ИСТОЧНИК: MSDN AttachThreadInput]:**
   - Временное связывание очереди ввода текущего потока и потока целевого окна.
   - **КРИТИЧЕСКИЙ РИСК:** Если целевой процесс завис (дедлок, ожидание сети/диска), поток тайлинг-менеджера **намертво зависает** внутри своего цикла сообщений. Вызывает десинхронизацию состояния клавиатуры и потерю событий.
2. **Симуляция нажатия `Alt` (`keybd_event` / `SendInput` с `VK_MENU`):**
   - Генерация фиктивного нажатия Alt снимает блокировку фокуса.
   - **РИСК:** Залипание состояния клавиши Alt, непроизвольное открытие системных меню в сторонних приложениях, срыв пользовательского набора текста.
3. **Легальный архитектурный путь для Tiling WM:**
   - Навигация по тайлам (переключение фокуса) инициируется **глобальным хоткеем** (`Alt+H/J/K/L` или `Win+H/J/K/L`).
   - Поскольку глобальный хоткей обрабатывается `RegisterHotKey` в потоке resticker, Windows считает resticker **получателем аппаратного ввода (hardware input recipient)**.
   - В этом состоянии resticker **легально обладает правом** вызывать `SetForegroundWindow(target_hwnd)` без каких-либо хаков и рисков зависания!

---

## 7. Элевейтед-окна и UIPI (User Interface Privilege Isolation)

### 7.1 Механика UIPI

[ИСТОЧНИК: MSDN User Interface Privilege Isolation]:
UIPI блокирует взаимодействие процессов с более низким уровнем целостности (Integrity Level) с процессами более высокого уровня:
- Непривилегированный процесс (Medium Integrity) не может отправлять оконные сообщения (`WM_COMMAND`, `WM_SYSCOMMAND`), вызывать `SetWindowPos`, `SetWindowPlacement` или модифицировать оконные свойства (`SetPropW`) окон администратора (Task Manager, Regedit, elevated cmd/PowerShell, IDE с правами админа).
- Вызовы `SetWindowPos` возвращают `ERROR_ACCESS_DENIED` (код 5).

### 7.2 Сравнение режимов работы

| Режим запуска resticker | Возможности тайлинга | Ограничения и проблемы |
|---|---|---|
| **Без прав администратора (Medium Integrity)** | Тайлит 95% обычных пользовательских окон (браузеры, мессенджеры, проводник). | Не может сдвинуть или изменить размер окон администратора (`PinAccessDenied`). Окна админа выпадают из сетки. |
| **С правами администратора (High Integrity)** | Беспрепятственно тайлит **все** окна в пользовательской сессии без исключений. | Drag-and-Drop файлов из проводника в окно настроек resticker блокируется UIPI (требуется `ChangeWindowMessageFilterEx`). |

### 7.3 Реализация в resticker

В resticker перехват UIPI-ограничений полностью реализован:
- Ошибки `ERROR_ACCESS_DENIED` трансформируются в `Win32Error::PinAccessDenied` ([crates/rst-win32/src/window_pin.rs:348-350](file:///C:/resticker/crates/rst-win32/src/window_pin.rs#L348-L350)).
- Описание ошибки в [crates/rst-win32/src/error.rs:39-42](file:///C:/resticker/crates/rst-win32/src/error.rs#L39-L42) явно указывает причину:
  `"pinning was rejected by the system: this process has no access to the target window (UIPI). Restart resticker as administrator to pin stickers to windows running with elevated rights"`
- Координатор отлавливает эту ошибку и показывает всплывающее уведомление пользователю ([crates/resticker/src/overlay_manager.rs:7782](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L7782)).

---

## 8. ЧТО МЕНЯ БЕСПОКОИТ (Ключевые риски и подводные камни)

1. **Асинхронный рендеринг сторонних процессов и лаг ресайза:**
   В X11/Wayland композитор держит буферы окон и может синхронно заморозить перерисовку. В Win32 при динамическом перетаскивании разделителя тайлов (split resize) 5 процессов одновременно получают `WM_SIZE` и пересоздают swapchain с разной скоростью. Это неизбежно приведет к микро-фризам и кратковременным черным полосам у тяжелых приложений (Chromium, Discord, IDE).
2. **Приложения с кастомными рамками (CSD / Client-Side Decorations):**
   Приложения на базе Electron, Chromium, WPF, Custom DWM (Steam, Spotify, Discord, VS Code) сами обрабатывают `WM_NCHITTEST` и вычисляют неклиентскую область через `DwmExtendFrameIntoClientArea`. У многих из них жестко прописаны минимальные размеры `ptMinTrackSize`, из-за чего они отказываются ужиматься в компактные тайлы и вылезают за гэпы.
3. **Хрупкость и недокументированность Virtual Desktops API:**
   Официальный публичный COM-интерфейс `IVirtualDesktopManager` ([crates/rst-win32/src/virtual_desktops.rs:32](file:///C:/resticker/crates/rst-win32/src/virtual_desktops.rs#L32)) умеет только проверять `IsWindowOnCurrentVirtualDesktop`. Перемещение окон между виртуальными столами и перечисление GUID столов требует недокументированного `IVirtualDesktopManagerInternal`, чьи vtable-структуры ломаются почти в каждом минорном обновлении Windows 11.
4. **Зависание UI-потоков чужих окон и модальные циклы:**
   Когда пользователь случайно или намеренно зажимает заголовок чужого окна, Win32 входит в системный модальный цикл `DefWindowProc` (Move/Size Loop). В этот момент приложение блокирует синхронные запросы. Если в коде тайлинга появится хоть один синхронный `SendMessageW` вместо `SendMessageTimeoutW` / `PostMessageW`, resticker зависнет вместе с чужим окном.
5. **Конфликты с играми и Exclusive Fullscreen:**
   Попытка тайлить или изменять Z-order окон игр в режиме Exclusive Fullscreen / Borderless приводит к сбросу графического устройства DirectX (`DXGI_ERROR_DEVICE_RESET`) и вылету игры. Тwm обязан жестко детектировать Fullscreen-приложения и автоматически переводить их в режим игнорирования.
6. **Дилемма прав администратора (UIPI UX Trap):**
   Если запускать resticker обычным пользователем — пользователи будут жаловаться на неработающий тайлинг Диспетчера задач и терминалов. Если запускать всегда от администратора — ломается автозапуск из реестра `Run` (UAC блокирует автостарт без Task Scheduler) и перестает работать Drag-and-Drop картинок/стикеров из стандартного проводника.
