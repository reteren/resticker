# R2 — Разбор чужого опыта: тайлинг на Windows и чем Hyprland отличается

Статус: исследование внешнего мира, код репозитория не трогали.
Метки: `[ИСТОЧНИК: URL]` — проверено по источнику; `[ПРЕДПОЛОЖЕНИЕ]` — вывод на основе прочитанного, не проверял вживую; `[ИЗМЕРЕНО]` — эмпирический факт.
Все коммиты/файлы чужих проектов указаны по состоянию на авг 2026 (глубина поиска).

---

## 1. Обзор проектов: как они устроены

### 1.1 komorebi (Rust) — [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi, https://deepwiki.com/LGUG2Z/komorebi/3-architecture]

- **Архитектура**: клиент-сервер. `komorebi.exe` — постоянный процесс; `komorebic` — CLI, шлёт команды через сокеты (Unix domain + опционально TCP); третья сторона (AutoHotKey/whkd) вешает горячие клавиши. Бинды в самом komorebi НЕ живут — по образцу bspwm/yabai. [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi]
- **Цикл управления**: событийный. Один глобальный `SetWinEventHook` (модуль `winevent_listener.rs`, старт на стр. 24–37), коллбек `win_event_hook` (`windows_callbacks.rs:81–150`) фильтрует по `OBJID_WINDOW`, отбрасывает `WS_CHILD`/`WS_EX_TOOLWINDOW`/`WS_EX_NOACTIVATE` (`windows_callbacks.rs:72–79`). События → каналы crossbeam → отдельный поток `listen_for_events` (`process_event.rs:122–139`), ядро под `Arc<Mutex<WindowManager>>`. [ИСТОЧНИК: https://deepwiki.com/LGUG2Z/komorebi/3.3-event-processing-system]
- **Чем ловит окна**: ObjectShow/ObjectHide/ObjectDestroy/ObjectFocus/SystemForeground/SystemMinimizeStart/SystemMoveSizeStart/SystemMoveSizeEnd/ObjectCloaked/ObjectUncloaked/ObjectNameChange. Спец-кейс: Firefox не шлёт ObjectShow — ловят по ObjectNameChange (`window_manager_event.rs:171–214`). [ИСТОЧНИК: deepwiki, там же]
- **Иерархия**: WindowManager → Ring\<Monitor\> → Ring\<Workspace\> → Ring\<Container\> → Window. Ring-буфер даёт циклический фокус. [ИСТОЧНИК: https://deepwiki.com/LGUG2Z/komorebi/3-architecture]
- **Как двигает**: `SetWindowPos` (обёртка в `windows_api.rs`; не проверял строку). Анимация = спам вызовов SetWindowPos с интерполяцией (`animation {enabled, duration=250ms, fps=60, style Linear|EaseOutSine}`), только внутри одного монитора-воркспейса, "не стабильна, возможны артефакты". [ИСТОЧНИК: https://lgug2z.github.io/komorebi/common-workflows/animations.html] Автор прямо пишет: "this is always going to be dependent on user hardware until Microsoft provides first class animation APIs so that we don't have to emulate animation by spamming calls to update window positions" [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi/issues/1541].
- **Рамка активного окна**: отдельные нативные окна-бордеры (по одному HWND на рамку, Direct2D `ID2D1HwndRenderTarget`), держатся поверх через `HWND_TOPMOST`/`HWND_TOP`, синхронизируются с движением окна сообщением `WM_ANIMATE_RECT` (`border_manager/border.rs:608–615`, `mod.rs:119–128`). [ИСТОЧНИК: https://deepwiki.com/LGUG2Z/komorebi/6.1-visual-feedback-systems]
- **Гэпы**: `workspace-padding`/`container-padding` (внутренние/внешние), настраиваются per-workspace, есть `global-work-area-offset`. [ИСТОЧНИК: https://lgug2z.github.io/komorebi/cli/window-hiding-behaviour.html (список команд)]
- **Воркспейсы**: СВОИ, не системные (подробно в разделе 2).
- **Известные проблемы** (issue-трекер):
  - Клоак не прячет окна на чужих виртуальных столах [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi/issues/1697];
  - Задержка анимации при открытии окна [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi/issues/1541];
  - Elevated Windows Terminal не тайлится [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi/issues/1237];
  - Tray-приложения: закрытие в трей оставляет пустой тайл [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi/issues/6];
  - `hide` (SW_HIDE) ломает Electron-приложения → режим EOL, минимизация "имеет проблемы при частом переключении" → рекомендован cloak через undocumented `SetCloak` [ИСТОЧНИК: https://lgug2z.github.io/komorebi/cli/window-hiding-behaviour.html];
  - Пользователи жалуются на срывы с существующими окнами при старте (не тайлит уже открытые) и боль на установке [ИСТОЧНИК: https://www.makeuseof.com/i-replaced-windows-snap-layouts-with-a-tiling-window-manager-and-got-the-linux-experience-i-wanted].

### 1.2 GlazeWM (C# → Rust) — [ИСТОЧНИК: https://github.com/glzr-io/glazewm, https://deepwiki.com/glzr-io/glazewm]

- **Архитектура**: исторически C# (lars-berger), сейчас рерайт на Rust под org glzr-io: workspace crates `wm` (главный процесс), `wm-cli`, `wm-watcher` (восстановление окон после краха основного процесса!), `wm-platform` (Win32-слой), IPC через WebSocket. [ИСТОЧНИК: https://deepwiki.com/glzr-io/glazewm/1-overview]
- **Цикл управления**: `tokio::select!` мультиплексирует 5 источников: mouse_listener, window_listener, keybinding_listener, ipc_server, tray (`packages/wm/src/main.rs:188–217`). [ИСТОЧНИК: там же]
- **Чем ловит окна**: `SetWinEventHook` (файл `packages/wm/src/common/platform/window_event_hook.rs`) + первичное сканирование `EnumWindows` (`native_window.rs:885–914`); фильтр `is_manageable` по стилям/классам. [ИСТОЧНИК: https://deepwiki.com/glzr-io/glazewm/3.3.6-native-window-abstraction]
- **Как двигает и что умеет с окном** (все в `packages/wm-platform/src/native_window.rs`): `SetWindowPos` (z-order, 541–554), фокус `SetForegroundWindow` (296–318), рамка — `DwmSetWindowAttribute(DWMWA_BORDER_COLOR)` (320–340, **только Windows 11**), скругления `DWMWA_WINDOW_CORNER_PREFERENCE`, скрытие тайтлбара через `WS_DLGFRAME`+`SetWindowLongPtrW` (366–403), прозрачность `SetLayeredWindowAttributes(LWA_ALPHA)` (419–443), клокинг `DWMWA_CLOAKED` (624–645), таскбар `ITaskbarList::AddTab/DeleteTab` (647–670), видимый rect через `DWMWA_EXTENDED_FRAME_BOUNDS` (445–467). [ИСТОЧНИК: https://deepwiki.com/glzr-io/glazewm/3.3.6-native-window-abstraction]
- **Гэпы**: `gaps { inner_gap, outer_gap, scale_with_dpi }`. [ИСТОЧНИК: https://gist.github.com/zypeaLLas/26aac42471ac747a18f80582fc3b6dc3]
- **Воркспейсы**: свои + скрытие окон при переключении (`hide_method: cloak|hide`), `show_all_in_taskbar` для управления таскбаром. [ИСТОЧНИК: тот же gist]
- **Бинды**: встроенные, YAML `keybindings` + `binding_modes` (аналог hyprland submaps). [ИСТОЧНИК: https://github.com/glzr-io/glazewm README]
- **Анимации**: в работе, PR #1199. [ИСТОЧНИК: https://blog.markvincze.com/switching-to-the-glazewm-tiling-window-manager-on-windows]
- **Известные проблемы**:
  - Конфликт с нативными Windows Desktops — лейаут схлопывается [ИСТОЧНИК: https://github.com/glzr-io/glazewm/issues/1211];
  - Флоатящие окна все разом выходят наверх при активации одной (z-order vs DWM) [ИСТОЧНИК: https://github.com/glzr-io/glazewm/issues/1055];
  - Elevated-окна не управляются без uiAccess-подписи exe в Program Files [ИСТОЧНИК: https://github.com/glzr-io/glazewm/issues/867].

### 1.3 FancyWM (C#/.NET) — [ИСТОЧНИК: https://github.com/FancyWM/fancywm]

- Событийная модель на базе библиотеки WinMan (платформо-независимая обёртка над Win32: хуки, EnumWindows, сообщения) — [ИСТОЧНИК: https://github.com/veselink1/winman-windows]. Классический подход: Hook WinEvents → свой message loop. [ПРЕДПОЛОЖЕНИЕ на основе состава репозитория]
- Фичи: панели (горизонталь/вертикаль/стек), two-pass layout-алгоритм, mouse+keyboard, авто-флоат транзиентов, виртуальные десктопы (понимает нативные Windows VD), подсветка фокуса "blink", анимации отключаемы. [ИСТОЧНИК: https://github.com/FancyWM/fancywm README]
- Важно для нас: был коммерческим (2022), сейчас MIT open source. [ИСТОЧНИК: https://fancywm.github.io/fancywm/, https://news.ycombinator.com/item?id=29799152]

### 1.4 Whim (C#/.NET, WinUI 3) — [ИСТОЧНИК: https://github.com/dalyIsaac/Whim]

- Плагинная архитектура, layout engines (SliceLayoutEngine ≈ dynamic tiling, TreeLayoutEngine ≈ i3-дерево), YAML/JSON конфиг + C#-скрипты, командная палитра, bar. [ИСТОЧНИК: README]
- **Ключевая цитата**: "Whim does not use Windows' native 'virtual' desktops, as they lack the ability to activate 'desktops' independently of monitors. Instead, Whim has workspaces." [ИСТОЧНИК: https://github.com/dalyIsaac/Whim README] — фактически та же причина, что и у всех.
- Sticky-воркспейсы (привязка к конкретным мониторам), сохранение состояния между сессиями. [ИСТОЧНИК: https://dalyisaac.github.io/Whim/configure/core/workspaces.html]
- Потребитель upstream-конфига приложений komorebi (общий список "плохих" приложений). [ИСТОЧНИК: https://news.ycombinator.com/item?id=41547053]

### 1.5 workspacer (C#) — [ИСТОЧНИК: https://github.com/workspacer/workspacer, https://workspacer.org]

- Конфигурация целиком на C# (csx-скрипты), Win32-слой вынесен в workspacer.Native (WindowsManager: discovery + event hooks, события create/destroy/focus/move). [ИСТОЧНИК: https://deepwiki.com/workspacer/workspacer/3.3-native-windows-integration]
- Явно декларирует: "doesn't use DLL injection to manipulate windows, so it less likely to break things" — и честно: "deviates where not possible due to limitations of the Win32 API which prevents from freely controlling windows in the same way as an X11 tiling window manager". [ИСТОЧНИК: https://workspacer.org/quickstart/]
- MIT. [ИСТОЧНИК: там же]

### 1.6 PowerToys FancyZones (C++/WinUI) — [ИСТОЧНИК: https://learn.microsoft.com/en-us/windows/powertoys/fancyzones]

- Не тайлер, а "зоны": пользователь рисует зоны (Grid/Canvas), окно перетаскивается в зону или двигается Win+стрелками. [ИСТОЧНИК: MS Learn]
- Механика: `SetWinEventHook` на 7 событий (EVENT_SYSTEM_MOVESIZESTART/END, EVENT_OBJECT_NAMECHANGE, UNCLOAKED, SHOW, CREATE, LOCATIONCHANGE) → преобразование в сообщения → свой WndProc; метаданные окна (прошлая зона, размер) хранятся в window properties. [ИСТОЧНИК: https://samrambles.com/guides/fancyzones/how-fancyzones-works/index.html]
- Ограничение: зоны через мониторы требуют одинакового DPI scaling. [ИСТОЧНИК: MS Learn]
- Известный баг-класс: FancyZones пересобирает окна после ручного ресайза [ИСТОЧНИК: https://github.com/microsoft/PowerToys/issues/34016].
- MIT (весь PowerToys). [ИСТОЧНИК: https://github.com/microsoft/PowerToys]

### Общий вывод по архитектуре

Все шесть проектов делают одно и то же: **SetWinEventHook (внеконтекстный) → свой event loop → дерево окон/контейнеров → SetWindowPos для применения лейаута**. Различия — только в языке, IPC и наборе косметики (бордеры отдельными окнами у komorebi, DWM-атрибуты у GlazeWM, window properties у FancyZones). Ни один не имеет доступа к композиции — все "сверху" DWM. [ПРЕДПОЛОЖЕНИЕ, подтверждено README всех проектов]

---

## 2. Воркспейсы: как komorebi и GlazeWM обходят IVirtualDesktopManager

**Почему проблема есть**: публичный `IVirtualDesktopManager` (CLSID_VirtualDesktopManager) умеет только `GetWindowDesktopId`, `IsWindowOnCurrentVirtualDesktop`, `MoveWindowToDesktop` — перечисления столов нет. [ИСТОЧНИК: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager] Более того, `MoveWindowToDesktop` на окнах чужих процессов возвращает `E_ACCESSDENIED` (проверено на explorer/skype/firefox; "Launch as admin" не помогает). [ИСТОЧНИК: https://stackoverflow.com/questions/32659505] То есть официальный API для менеджера окон бесполезен.

Есть три пути, и по ним все и делятся:

**Путь A. Свои воркспейсы = свой стек + прятать окна.** komorebi и GlazeWM (и Whim) хранят воркспейсы сами (композиция монитор→воркспейс→контейнер), а при переключении скрывают окна неактивного воркспейса тремя способами:
1. `SW_HIDE` (`hide`) — грубо, ломает Electron (окно "умирает" для приложения), помечен EOL у komorebi; [ИСТОЧНИК: https://lgug2z.github.io/komorebi/cli/window-hiding-behaviour.html]
2. `SW_MINIMIZE` (`minimize`) — окна исчезают из таскбара/Alt-Tab, "проблемы при частом переключении"; [там же]
3. cloak: undocumented `SetCloak()` (komorebi, из AltTabAccessor) или документированный аналог `DwmSetWindowAttribute(DWMWA_CLOAKED)` (GlazeWM, `native_window.rs:624–645`) — окно остаётся "живым", DWM его не рисует и не показывает в Task View/переключателях, но оно всё ещё в таскбаре (есть флаг `show_all_in_taskbar` у GlazeWM). [ИСТОЧНИК: gist конфига GlazeWM; deepwiki GlazeWM 3.3.6]

Плюсы пути A: полный контроль (воркспейсы per-monitor, чего нативные столы не умеют — в Windows "Displays Have Separate Virtual Desktops" так и не выпустили [ИСТОЧНИК: https://superuser.com/a/1783505]); мгновенное переключение без анимаций ОС; именованные воркспейсы (komorebi named workspaces).
Минусы: это **виртуальные столы в самом себе** — вся экосистема (Task View, Alt-Tab, Win+Tab, Explorer-интеграция, "Show on all desktops") не знает про твои воркспейсы; окна на чужих нативных столах не прячутся (баг komorebi #1697); при крахе процесса окна остаются скрытыми → GlazeWM вынужден держать watcher-процесс, который восстанавливает окна [ИСТОЧНИК: deepwiki GlazeWM 1-overview]; некоторым приложениям не нравится быть cloaked.

**Путь B. Приватные COM-интерфейсы к Explorer.** `IVirtualDesktopManagerInternal`, `IApplicationViewCollection`, `IVirtualDesktop` — живут в `explorer.exe` (CLSID VirtualDesktopManagerInternal), **не документированы**, vtable меняется между билдами Windows; известные обёртки: VirtualDesktopAccessor (C++/AHK, Ciantic), VirtualDesktop (C#/WPF, Grabacr07), zVirtualDesktop (документирует версии интерфейсов). [ИСТОЧНИК: https://stackoverflow.com/questions/32659505] Используют VirtuaWin/Actual Tools (платные) и разовый код. GlazeWM держит feature-request #671 "use Virtual Desktops instead of window hiding" — но не реализует, остаётся на пути A. [ИСТОЧНИК: https://github.com/glzr-io/glazewm/issues/671]
Плюсы: нативные столы видны в Task View/Alt-Tab, дешевле держать экосистему. Минусы: хрупкость (приватные vtable, ломаются на каждой мажорной версии Win11), без enumeration всё равно не обойтись без этих интерфейсов, нельзя перечислять "окна на столе N" штатно даже для чтения; юридически это серый реверс-инжиниринг (для нас — риск поддержки).

**Путь C. Комбинированный.** FancyWM "понимает" нативные VD (перемещение окон между ними через IVirtualDesktopManager на свои окна не работает — E_ACCESSDENIED... но FancyWM заявляет "Virtual desktop awareness and movement of windows between desktops" [ИСТОЧНИК: README], вероятно через приватные интерфейсы пути B [ПРЕДПОЛОЖЕНИЕ]).

**Вердикт для resticker**: путь A (свой стек + cloak) — единственный, который даёт per-monitor воркспейсы и не зависит от билда Windows. Это же решение комитета всех крупных игроков. Цена — своя "песочница" воркспейсов и забота о восстановлении окон при падении (watcher). [ПРЕДПОЛОЖЕНИЕ]

---

## 3. Фичи Hyprland → вердикт для Windows

Ссылки на фичи: [ИСТОЧНИК: https://wiki.hypr.land/Configuring/Basics/Variables/, /Configuring/Basics/Binds/, /Configuring/Basics/Dispatchers/, /Configuring/Basics/Window-Rules/, /Configuring/Basics/Workspace-Rules/, /Configuring/Layouts/Dwindle-Layout/, /Configuring/Layouts/Master-Layout/, /Configuring/Advanced-and-Cool/Animations/] — см. также конфиг из issue #8858 как сводку опций.

| Фича Hyprland | Вердикт для Windows | Оговорка |
|---|---|---|
| **Dwindle layout** (псевдо-дерево, умные сплиты) | ✅ реализуемо как есть | Это чистая математика над прямоугольниками; komorebi BSP/стек доказывают. Придётся самому писать дерево или брать идею из komorebi/Whim TreeLayoutEngine. |
| **Master layout** | ✅ реализуемо как есть | То же; master+slave раскладки есть даже в FancyWM (панели). |
| **gaps_in / gaps_out** | ✅ реализуемо как есть | komorebi: container/workspace padding; GlazeWM: inner_gap/outer_gap + scale_with_dpi. Нюанс: гэп у чужих окон рисуется "дырой", а не композитной тенью — фон просвечивает (нормально). |
| **Groups / tabbed** | ✅ реализуемо как есть, с оговоркой | komorebi имеет стеки + stackbar (GDI-вкладки, свои окна) [ИСТОЧНИК: deepwiki 6.1]; FancyWM stack panels; Whim... Группа = контейнер + переключение видимости внутри контейнера (minimize/cloak остальных + своя панель вкладок). Оговорка: чужие окна нельзя перекрашивать в таб-баре без отдельного UI-слоя; коммит истории: mouse-взаимодействие со stackbar у komorebi ограничено click-to-focus. |
| **Special workspace / scratchpad** | ✅ реализуемо как есть | Это просто дополнительный воркспейс "поверх" (показ поверх текущего = SetWindowPos HWND_TOPMOST + временная отмена cloak). Нет ни одного технического препятствия. |
| **Submaps (режимы биндов)** | ✅ реализуемо как есть | GlazeWM `binding_modes` уже делает это в конфиге [ИСТОЧНИК: README GlazeWM]. |
| **Window rules** (match class/title + свойства) | ✅ реализуемо как есть, с оговоркой | Win32 даёт exe/class/title/HWND-атрибуты; komorebi собрал целый upstream "плохих приложений" [ИСТОЧНИК: HN]. Оговорка: соответствие "типу окна" (dialog/transient) сложнее — надо крутить GWL_STYLE/догадки по owned-окнам. |
| **Resize/move binds** (resizeactive, movewindow, resize deltas) | ✅ реализуемо как есть | У GlazeWM есть `resize --width/height %`; у komorebi `resize-edge/resize-axis/resize-delta`. |
| **Animations + bezier-кривые** | ⚠️ реализуемо частично, оговорка: нет API композиции | Двигать окно = таймер + серия SetWindowPos. Дрожь/мигание зависят от железа и приложения; komorebi сам признаёт артефакты и CPU-нагрузку [ИСТОЧНИК: issue #1541, docs]. Безье легко: та же математика кривой, что у Hyprland. Скольжение контента окна (slide/попин с масштабом) — НЕЛЬЗЯ: мы не рендерим содержимое чужих окон. Максимум — анимировать позицию/размер прямоугольника. |
| **Blur / opacity (decoration)** | ⚠️ частично | Opacity: `SetLayeredWindowAttributes(LWA_ALPHA)` — есть у komorebi (transparency manager) и GlazeWM [ИСТОЧНИК: deepwiki 6.1 / 3.3.6]. Blur фона за окном: **невозможно** штатно — DWM не даёт рисовать между фоном и окном (нужен Desktop Duplication API + свой композитор = нереально для WMs; только для спец-приложений). |
| **Workspace swipe (жесты тачпада)** | ⚠️ частично, оговорка: нужен raw-input слой | Гипотетически: захват сенсорной панели через Raw Input API + переключение воркспейса (cloak/uncloak). Никто из шести проектов этого не делает [ПРЕДПОЛОЖЕНИЕ: не нашёл упоминаний]; конкуренция с системными жестами Windows (4-пальцевый swipe = свои столы) — война за жесты, которую штатно не выиграть без переопределения системных горячих клавиш. |
| **Fullscreen / monocle** | ✅ реализуемо | komorebi toggle-monocle/toggle-maximize; GlazeWM fullscreen. |
| **Floating поверх тайла** | ✅ реализуемо, с оговоркой | z-order флоатящих окон — больное место (GlazeWM #1055: все флоатящие разом наверх). |

---

## 4. Что на Windows принципиально хуже, чем на Wayland

1. **Нет доступа к композиции.** Композитор — DWM, закрытый и неуправляемый. Нельзя: рисовать под/между окнами (кроме слоёв вроде прозрачных оверлеев — что resticker уже делает через D3D11+DirectComposition, но это наш слой, а не тайлинг), анимировать контент окна, блюрить фон, управлять декором чужих окон. Wayland-композитор — единый источник истины по слоям; Windows WM — гость на чужой сцене. [ИСТОЧНИК: общий вывод из README всех проектов; https://github.com/LGUG2Z/komorebi/issues/1541]
2. **Окна перерисовываются сами, с лагом.** После SetWindowPos приложение само решает, когда и как перерисоваться; возможны артефакты, "белые вспышки", задвоение теней (DWM кэширует тень по старому rect). Известный класс проблем: мерцание при программном ресайзе через SetWindowPos (DWM bitblt старый буфер) [ИСТОЧНИК: https://stackoverflow.com/questions/50898990].
3. **Приложения сопротивляются ресайзу.** Минимальные размеры (WM_GETMINMAXINFO), запрет ресайза (WS_THICKFRAME отсутствует), окна фиксированного размера, главные окна, которые сами себя позиционируют (Firefox — Whim имеет целый "Window Processor" для Firefox, который игнорирует его попытки перепозиционироваться [ИСТОЧНИК: https://dalyisaac.github.io/Whim/api/Whim.html]).
4. **Elevated / UIPI.** Окно с Integrity High (Run as administrator) нельзя двигать/писать в него сообщения из процесса с нормальной целостностью (UIPI). Решения: работать из-под администратора (плохо для юзера), или UIAccess-подпись exe в Program Files (GlazeWM), что имеет свои ограничения и не всегда работает [ИСТОЧНИК: https://github.com/glzr-io/glazewm/issues/867, https://en.wikipedia.org/wiki/User_Interface_Privilege_Isolation].
5. **DPI per-monitor.** Смешанные scaling-мониторы ломают "одну координатную сетку": окно при перемещении между мониторами переживает ре-scaling (окно пересоздаёт свой контент); метрики надо пересчитывать в DIP; FancyZones прямо запрещает зоны через мониторы с разным scaling [ИСТОЧНИК: MS Learn FancyZones]. GlazeWM добавляет `scale_with_dpi` для гэпов [ИСТОЧНИК: gist].
6. **Snap Layouts (Win11) и системный snap.** ОС активно вмешивается в перемещение окон (drag-to-top-зацепка, Win+стрелки, snap layout popup при наведении на maximize) — тайлер и snap воюют за одни и те же действия пользователя; "лечится" только отключением в Settings/Multitasking или реестром (Win11 22H2 build 22621.1344+), но это требование к юзеру [ИСТОЧНИК: https://www.ninjaone.com/blog/how-to-enable-or-disable-snap-layouts/, https://www.howtogeek.com/743536/how-to-turn-off-snap-layouts-in-windows-11/]. Также Windows Desktops конфликтуют (GlazeWM #1211).
7. **Экосистема окна не знает о WM.** Task View, Alt-Tab, Win+Tab, "Show on all desktops", нативные VD — всё это игнорирует наши воркспейсы; обратная совместимость достигается только костылями (ITaskbarList AddTab/DeleteTab у GlazeWM).

---

## 5. Лицензии

| Проект | Лицензия | Что это значит для нас |
|---|---|---|
| komorebi | **Komorebi 2.0.0** (форк PolyForm Strict 1.0.0): только личное использование, запрет редистрибуции и хард-форков, запрет коммерческого использования и использования некоммерческими организациями; есть платная Individual Commercial License | ⚠️ **Код komorebi копировать нельзя вообще** (ни в продукт, ни в OSS-репозиторий). Идеи/архитектуру можно — идеи лицензией не охраняются, но аккуратно: "позаимствовать алгоритм" из его кода = производная работа [ИСТОЧНИК: https://github.com/LGUG2Z/komorebi-license, https://lgug2z.github.io/komorebi/index.html] |
| GlazeWM | MIT (v3.0+) | ✅ Можно брать код/идеи с сохранением copyright notice [ИСТОЧНИК: https://glazewm.com/ "MIT License"] |
| workspacer | MIT | ✅ [ИСТОЧНИК: https://workspacer.org "© Licence MIT"] |
| Whim | MIT | ✅ [ИСТОЧНИК: https://github.com/dalyIsaac/Whim] |
| FancyWM | MIT (был коммерческим до ~2024) | ✅ [ИСТОЧНИК: https://fancywm.github.io/fancywm "MIT License"] |
| PowerToys/FancyZones | MIT | ✅ [ИСТОЧНИК: https://github.com/microsoft/PowerToys] |
| Hyprland | BSD-3-Clause | ✅ Можно брать код композитора (например, алгоритмы dwindle/анимаций) с указанием копирайта; но почти весь его код завязан на Wayland и для нас бесполезен как код, полезен как спецификация поведения [ИСТОЧНИК: https://github.com/hyprwm/Hyprland LICENSE — не проверял содержимое, стандарт проекта BSD-3; пометить как [ПРЕДПОЛОЖЕНИЕ]] |
| VirtualDesktopAccessor (Ciantic) | MIT | ✅ если пойдём в приватные COM-интерфейсы — код обёрток можно взять [ПРЕДПОЛОЖЕНИЕ: типовой MIT у этого класса тулзов; сам файл не открывал] |

---

## 6. ЧТО МЕНЯ БЕСПОКОИТ

1. **Потолок анимаций.** "Hyprland-уровень" анимаций на Windows = спам SetWindowPos, и даже автор komorebi признаёт это безнадёжным без нативного API (issue #1541). Если заявленная фича — плавность уровня Hyprland, ожидания пользователей будут разбиты о DWM. Надо либо честно ограничиться анимацией позиции/размера с умеренным fps, либо не обещать "плавность композитора".
2. **Крах процесса = скрытые окна.** Cloak-воркспейсы: если наш процесс упадёт с залоченными скрытыми окнами (а resticker — Tauri, у него есть свои точки падения), пользователь получит "потерянные" окна. GlazeWM держит отдельный watcher именно для этого. Это обязательный компонент, а не опция.
3. **UIPI и природа resticker.** resticker сейчас, судя по контексту, обычный пользовательский процесс. Тайлинг потребует либо админ-прав (плохо), либо UIAccess-подписи (сложно, сертификаты, Program Files), либо молчаливой невозможности управлять elevated-окнами (компромисс, но юзеры ругаются — см. issue #867). Надо решить политику заранее.
4. **Война с ОС за жесты и snap.** Snap Layouts, Win+стрелки, системные свайпы тачпада, нативные VD — ОС не уступит сама; каждый конфликт = ещё один пункт в документации "отключите в настройках Windows". Продуктовый риск: "не работает из коробки".
5. **Экосистема воркспейсов отрезана от Task View/Alt-Tab.** Пользователи, привыкшие к Win+Tab, получат второй, параллельный мир. Это не баг, это фундаментальная плата пути A, но многие воспримут как недоделку.
6. **Зоопарк приложений.** Electron/модные приложения игнорируют правила Win32: не шлют ObjectShow (Firefox), живут в трее, перепозиционируют себя сами. komorebi годами собирал upstream-конфиг "плохих приложений" и всё равно сыпется (issue #6, #1237). Для нас это означает отдельную инфраструктуру правил и бесконечный поток issue.
7. **Риск "второй komorebi".** Объём "полноценный WM уровня Hyprland" — это годы работы комьюнити komorebi/GlazeWM. Наш стартовый слой (M6, стикеры-окна) покрывает маленькую часть этого. Если фичу не срезать до ядра (воркспейсы + 2 лейаута + гэпы + бинды + border), есть риск выпустить недоделанный тайлер, который хуже любого из существующих бесплатных. Дифференциатор должен быть не "ещё один тайлер", а связка со стикерами/оверлеями resticker.

---

## 7. Полезные ссылки (сводно)

- komorebi: https://github.com/LGUG2Z/komorebi · https://lgug2z.github.io/komorebi · deepwiki.com/LGUG2Z/komorebi
- GlazeWM: https://github.com/glzr-io/glazewm · deepwiki.com/glzr-io/glazewm
- FancyWM: https://github.com/FancyWM/fancywm · WinMan: https://github.com/veselink1/winman-windows
- Whim: https://github.com/dalyIsaac/Whim · https://dalyisaac.github.io/Whim
- workspacer: https://github.com/workspacer/workspacer · https://workspacer.org
- PowerToys FancyZones: https://learn.microsoft.com/en-us/windows/powertoys/fancyzones · разбор механики: https://samrambles.com/guides/fancyzones/how-fancyzones-works
- IVirtualDesktopManager: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager · приватные интерфейсы: https://stackoverflow.com/questions/32659505
- Hyprland wiki: https://wiki.hypr.land/Configuring/ (Variables, Binds, Dispatchers, Window-Rules, Workspace-Rules, Layouts/Dwindle, Layouts/Master, Advanced-and-Cool/Animations)
- Ключевые issue: komorebi #1541 (анимации), #1697 (cloak vs нативные столы), #1237 (elevated), GlazeWM #867 (elevated/UIAccess), #1211 (конфликт с Windows Desktops), #1055 (z-order флоата), #671 (feature request: нативные VD), FancyWM #34016 (у FancyZones ресайз).