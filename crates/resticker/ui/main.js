// resticker — окно настроек. Никакого бандлера/фреймворка (M0: "no runtime
// JS framework"), обычный скрипт — тот же принцип, что у прежнего
// пустого каркаса, просто с реальным содержимым вкладок.

const { invoke } = window.__TAURI__.core;
const { open, save } = window.__TAURI__.dialog;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow } = window.__TAURI__.window;

/** @type {any} последний загруженный/применённый Config с бэкенда. */
let config = null;
/** Черновики, которые ещё не отправлены в координатор (`Apply`/`OK`). */
let draftSettings = null;
let draftHotkeys = null;
/** Какое из трёх полей хоткея сейчас "слушает" следующую комбинацию клавиш. */
let recordingField = null;
/** id пресета, который сейчас переименовывается через поле `preset-name`
 *  (`null` — режим «сохранить как пресет»). */
let renameTargetId = null;

const statusEl = document.getElementById('status');
const stickerStatusEl = document.getElementById('sticker-status');
const presetStatusEl = document.getElementById('preset-status');
const denylistStatusEl = document.getElementById('denylist-status');

/** Иконка из спрайта в index.html. Своей разметки на месте вызова нет:
 *  иначе один и тот же путь пришлось бы держать в трёх шаблонах строк. */
function icon(name) {
  return `<svg aria-hidden="true"><use href="#i-${name}" /></svg>`;
}

// ==== Хоткей как клавиши ====
//
// Сочетание рисуется отдельными клавишами, а не строкой в поле ввода.
// Строкой оно ломалось об ширину поля: «Ctrl+Alt+(number of group)»
// обрезалось на «Ctrl+Alt+(number of grou» и хвост, который как раз и
// объясняет смысл бинда, до пользователя не доходил. Клавиши переносятся
// по строкам сами, а глаз находит нужный бинд, не читая.

/** Одна клавиша. `ghost` — не клавиша, а пояснение (подставной хвост
 *  «(number of group)», многоточие во время записи): у него нет рельефа,
 *  чтобы его не искали на клавиатуре. */
function keycap(label, ghost) {
  const span = document.createElement('span');
  span.className = ghost ? 'keycap ghost' : 'keycap';
  span.textContent = label;
  return span;
}

function appendCaps(el, parts, ghostLast) {
  parts.forEach((part, i) => {
    if (i > 0) {
      const plus = document.createElement('span');
      plus.className = 'keycapPlus';
      plus.textContent = '+';
      el.appendChild(plus);
    }
    el.appendChild(keycap(part, ghostLast && i === parts.length - 1));
  });
}

/** Показать сохранённое значение поля. Пустая строка — плейсхолдер. */
function setHotkeyDisplay(el, combo) {
  if (!el) return;
  el.replaceChildren();
  if (!combo) {
    const empty = document.createElement('span');
    empty.className = 'hotkeyEmpty';
    empty.textContent = t('control.notSet');
    el.appendChild(empty);
    return;
  }
  const parts = combo.split('+');
  // Хвост поля открытия группы — не клавиша: цифру пользователь не
  // выбирает, её присваивает сама группа (см. spreadDigits).
  const ghostLast = parts[parts.length - 1] === t('control.openGroupNumber');
  appendCaps(el, parts, ghostLast);
}

/** Показать состояние записи: уже зажатые модификаторы + ожидание. */
function setHotkeyRecording(el, mods) {
  if (!el) return;
  el.replaceChildren();
  if (!mods || mods.length === 0) {
    const empty = document.createElement('span');
    empty.className = 'hotkeyEmpty';
    empty.textContent = t('control.recording');
    el.appendChild(empty);
    return;
  }
  appendCaps(el, [...mods, '…'], true);
}

function setStatus(text) {
  statusEl.textContent = text;
  if (text) {
    setTimeout(() => {
      if (statusEl.textContent === text) statusEl.textContent = '';
    }, 4000);
  }
}

// ==== Вкладки ====

// Подложка активной вкладки — одна на всю ленту, и она переезжает.
// Положение и ширину CSS взять неоткуда (ширина вкладки зависит от текста
// перевода), поэтому их подаёт сюда JS в переменных --nav-x/--nav-w, а
// едет уже CSS-переход — на композиторе, не в главном потоке.
const tabsEl = document.getElementById('tabs');

function moveTabLight(animate = true) {
  const active = tabsEl.querySelector('.tab.active');
  if (!active) return;
  if (!animate) tabsEl.style.transition = 'none';
  tabsEl.style.setProperty('--nav-x', `${active.offsetLeft}px`);
  tabsEl.style.setProperty('--nav-w', `${active.offsetWidth}px`);
  if (!animate) {
    // Первая установка не должна выглядеть как переезд из левого угла.
    tabsEl.getBoundingClientRect();
    tabsEl.style.transition = '';
  }
}

tabsEl.addEventListener('click', (e) => {
  const tabEl = e.target.closest('.tab');
  const tab = tabEl?.dataset.tab;
  if (!tab) return;
  for (const b of document.querySelectorAll('.tab')) {
    b.classList.toggle('active', b.dataset.tab === tab);
  }
  for (const s of document.querySelectorAll('.panel')) {
    s.classList.toggle('active', s.dataset.panel === tab);
  }
  moveTabLight();
  updateScrollEdges();
  // Свежий снимок открытых окон при каждом заходе на вкладку — окна
  // открываются/закрываются, пока настройки открыты, список не должен
  // залипать на состоянии момента запуска (запрос пользователя 2026-08-19).
  if (tab === 'denylist') refreshDenylistProcessPicker();
});

// ==== Края прокрутки ====
//
// Вкладка «Общие» длиннее окна, и раньше об этом ничего не сообщало:
// последняя строка просто упиралась в подвал и обрывалась на полуслове,
// как будто вёрстка сломана. Теперь содержимое растворяется у того края,
// за которым оно продолжается, — и только у него.
const contentEl = document.querySelector('.content');

function updateScrollEdges() {
  const scrollable = contentEl.scrollHeight - contentEl.clientHeight;
  contentEl.classList.toggle('scroll-top', contentEl.scrollTop > 4);
  contentEl.classList.toggle('scroll-bottom', scrollable > 4 && contentEl.scrollTop < scrollable - 4);
}

contentEl.addEventListener('scroll', updateScrollEdges, { passive: true });
addEventListener('resize', () => {
  moveTabLight(false);
  updateScrollEdges();
});

// ==== Загрузка конфига ====

async function loadConfig() {
  try {
    config = await invoke('get_config');
  } catch (err) {
    setStatus(t('error.configLoad', { err }));
    config = { settings: {}, hotkeys: {}, stickers: [] };
  }
  draftSettings = { ...config.settings };
  draftHotkeys = { ...config.hotkeys };
  applyStaticTranslations();
  renderGeneral();
  renderControl();
  renderStickers();
  renderPresets();
  renderDenylist();
  refreshDenylistProcessPicker();
}

// ==== Вкладка «Общие» ====

function renderGeneral() {
  document.getElementById('autostart').checked = !!draftSettings.autostart;
  document.getElementById('silent-start').checked = !!draftSettings.silent_start;
  document.getElementById('tray-icon').checked = !!draftSettings.tray_icon;
  document.getElementById('skip-delete-confirmation').checked =
    !!draftSettings.skip_delete_confirmation;
  document.getElementById('hide-from-capture').checked = !!draftSettings.hide_from_capture;
  document.getElementById('never-overlap-taskbar').checked =
    !!draftSettings.never_overlap_taskbar;
  document.getElementById('outline-pinned-windows').checked =
    !!draftSettings.outline_pinned_windows;
  document.getElementById('snap-shrink-pct').value = draftSettings.snap_shrink_pct ?? 0;
  const fps = draftSettings.battery_fps_limit ?? 30;
  document.getElementById('battery-fps-limit').value = fps;
  document.getElementById('battery-fps-limit-value').textContent = String(fps);
  document.getElementById('mitosis-max-memory-mb').value =
    draftSettings.mitosis_max_memory_mb ?? 4096;
}

const GENERAL_CHECKBOXES = [
  ['autostart', 'autostart'],
  ['silent-start', 'silent_start'],
  ['tray-icon', 'tray_icon'],
  ['skip-delete-confirmation', 'skip_delete_confirmation'],
  ['hide-from-capture', 'hide_from_capture'],
  ['never-overlap-taskbar', 'never_overlap_taskbar'],
  ['outline-pinned-windows', 'outline_pinned_windows'],
];

for (const [elId, key] of GENERAL_CHECKBOXES) {
  document.getElementById(elId).addEventListener('change', (e) => {
    draftSettings[key] = e.target.checked;
  });
}

// Отступ в снап-зоне (SPEC 12, «Snap gap»): в отличие от подменю трея с его
// шагом в 5%, здесь значение вписывается числом — от этого и `type=number`,
// а не слайдер. Потолок держится и здесь, и в ядре: атрибут `max` умеет
// обойти любой, кто наберёт значение с клавиатуры и нажмёт Enter.
const snapGapInput = document.getElementById('snap-shrink-pct');
const SNAP_GAP_MAX = 35;
snapGapInput.addEventListener('input', () => {
  const raw = Number(snapGapInput.value);
  // Пустое поле и мусор — это 0, а не NaN: NaN уехал бы в конфиг и вернулся
  // бы оттуда `null`, обнулив настройку молча и не там, где её меняли.
  const pct = Number.isFinite(raw) ? Math.min(Math.max(Math.round(raw), 0), SNAP_GAP_MAX) : 0;
  draftSettings.snap_shrink_pct = pct;
});
// Правку показываем только после ухода из поля: подставлять кламп прямо во
// время набора значит вырывать курсор из-под пальцев на каждой цифре.
snapGapInput.addEventListener('blur', () => {
  snapGapInput.value = draftSettings.snap_shrink_pct ?? 0;
});

const fpsSlider = document.getElementById('battery-fps-limit');
const fpsValue = document.getElementById('battery-fps-limit-value');
fpsSlider.addEventListener('input', () => {
  fpsValue.textContent = fpsSlider.value;
  draftSettings.battery_fps_limit = Number(fpsSlider.value);
});

// Потолок приватной памяти для митоза (M9, settings.mitosis_max_memory_mb) —
// число, как snap-shrink-pct, а не слайдер: диапазон в 131072 ступени по
// 512 МБ ползунком не передать. Нижняя граница 512 МБ не случайна: митоз
// запускает ВТОРОЙ экземпляр приложения, и лимит ниже удвоенного веса
// лёгкого процесса отказывал бы всему подряд.
const mitosisMemoryInput = document.getElementById('mitosis-max-memory-mb');
const MITOSIS_MEMORY_MIN = 512;
const MITOSIS_MEMORY_MAX = 65536;
mitosisMemoryInput.addEventListener('input', () => {
  const raw = Number(mitosisMemoryInput.value);
  // Пустое поле и мусор — это дефолт 4096, а не NaN: NaN уехал бы в конфиг
  // и вернулся бы оттуда `null`, обнулив настройку молча и не там, где её
  // меняли (тот же приём, что у snap-shrink-pct).
  const mb = Number.isFinite(raw)
    ? Math.min(Math.max(Math.round(raw), MITOSIS_MEMORY_MIN), MITOSIS_MEMORY_MAX)
    : 4096;
  draftSettings.mitosis_max_memory_mb = mb;
});
// Правку показываем только после ухода из поля: подставлять кламп прямо во
// время набора значит вырывать курсор из-под пальцев на каждой цифре.
mitosisMemoryInput.addEventListener('blur', () => {
  mitosisMemoryInput.value = draftSettings.mitosis_max_memory_mb ?? 4096;
});

// ==== Вкладка «Управление» (хоткеи) ====

const HOTKEY_FIELDS = [
  ['hotkey-edit', 'edit_mode'],
  ['hotkey-toggle-all', 'toggle_all_stickers'],
  ['hotkey-mute-all', 'mute_all'],
  ['hotkey-pin', 'pin_focused_window'],
  ['hotkey-mitosis', 'window_mitosis'],
  ['hotkey-window-crop', 'window_crop'],
  ['hotkey-unpin-all', 'unpin_all'],
  ['hotkey-groups-menu', 'edit_groups_menu'],
  ['hotkey-delete-group', 'delete_open_group'],
  ['hotkey-pin-group', 'pin_open_group'],
  // Открытие группы — девять хоткеев, по одному на цифру, но настраивается
  // ОДНИМ полем: пользователь задаёт модификаторы, цифры подставляются сами.
  // Девять отдельных полей были бы стеной одинаковых строк, а разные
  // модификаторы у разных цифр никому не нужны.
  ['hotkey-open-group', 'open_group_by_number'],
];

// Поле открытия группы хранит МАССИВ из девяти строк, а не одну.
const OPEN_GROUP_FIELD = 'hotkey-open-group';
const GROUP_SLOTS = 9;

// Показать значение поля: массив цифровых хоткеев сворачивается в
// «модификаторы + подставной хвост» (см. groupDisplayValue) — остальные
// строки массива отличаются только цифрой.
function hotkeyFieldValue(key, value) {
  if (key !== 'open_group_by_number') return value ?? '';
  const first = Array.isArray(value) ? value.find((v) => v) : null;
  return first ? groupDisplayValue(first) : '';
}

// Конкретную цифру пользователь не выбирает никогда: каждая из девяти
// занята номером своей группы, и последняя часть бинда не редактируется.
// Показывать «Ctrl+Alt+1» как будто единица и есть бинд — враньё (репорт
// 2026-08-26), поэтому в показе цифра заменяется подставным хвостом из
// i18n. Строка без модификаторов (мусор в конфиге) даёт пустое значение —
// поле уходит в плейсхолдер, а не в «Ctrl+Alt+» с пустым хвостом.
function groupDisplayValue(combo) {
  const mods = combo.split('+').slice(0, -1);
  if (mods.length === 0) return '';
  return [...mods, t('control.openGroupNumber')].join('+');
}

// Раздать одни и те же модификаторы всем девяти цифрам.
function spreadDigits(combo) {
  const mods = combo.split('+').slice(0, -1);
  if (mods.length === 0) return null;
  return Array.from({ length: GROUP_SLOTS }, (_, i) => [...mods, String(i + 1)].join('+'));
}

function renderControl() {
  for (const [elId, key] of HOTKEY_FIELDS) {
    setHotkeyDisplay(document.getElementById(elId), hotkeyFieldValue(key, draftHotkeys[key]));
  }
  const volume = draftSettings.pin_sound_volume ?? 100;
  document.getElementById('pin-sound-volume').value = volume;
  document.getElementById('pin-sound-volume-value').textContent = String(volume);
}

// Громкость звука закрепления окна хоткеем (запрос пользователя
// 2026-08-19) — cfg.settings, не хоткей, но живёт визуально на этой
// вкладке рядом с самим хоткеем пина; тот же слайдер-паттерн, что
// battery-fps-limit в «Общие».
const pinVolumeSlider = document.getElementById('pin-sound-volume');
const pinVolumeValue = document.getElementById('pin-sound-volume-value');
pinVolumeSlider.addEventListener('input', () => {
  pinVolumeValue.textContent = pinVolumeSlider.value;
  draftSettings.pin_sound_volume = Number(pinVolumeSlider.value);
});

// Отменить запись текущего поля: вернуть его значение из draftHotkeys (а не
// оставить плейсхолдер "Нажмите комбинацию…" висеть) и снять .recording.
// Общий путь для Esc И для клика по ДРУГОМУ полю хоткея, пока это ещё
// записывает — раньше клик по другому полю чистил только CSS-класс, само
// значение оставалось залипшим на плейсхолдере (найдено независимым ревью).
function cancelRecording() {
  if (!recordingField) return;
  const [, key] = HOTKEY_FIELDS.find(([id]) => id === recordingField);
  const input = document.getElementById(recordingField);
  if (input) {
    setHotkeyDisplay(input, hotkeyFieldValue(key, draftHotkeys[key]));
    input.classList.remove('recording');
  }
  recordingField = null;
}

for (const [elId] of HOTKEY_FIELDS) {
  const input = document.getElementById(elId);
  const startRecording = () => {
    cancelRecording();
    recordingField = elId;
    input.classList.add('recording');
    setHotkeyRecording(input, []);
  };
  input.addEventListener('click', startRecording);
  // Поле перестало быть <input>, поэтому клавиатурный вход в запись нужно
  // задать самому: без этого до хоткеев нельзя было добраться с клавиатуры
  // вообще — что для окна, целиком посвящённого клавиатуре, странно.
  input.addEventListener('keydown', (e) => {
    if (recordingField) return; // запись уже идёт, комбинацию ловит общий обработчик
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      startRecording();
    }
  });
}

// Захватываем keydown/keyup в capture-фазе (третий аргумент true), чтобы
// системные или браузерные акселераторы и внутренние элементы не уводили фокус
// и события.
window.addEventListener(
  'keydown',
  (e) => {
    if (!recordingField) return;
    e.preventDefault();
    e.stopPropagation();

    if (e.code === 'Escape' || e.key === 'Escape') {
      cancelRecording();
      return;
    }

    // Если пользователь держит/нажимает модификаторы — показываем живой превью
    // ("Alt + Shift + …"), подтверждая, что клавиши распознаны и поле ждёт букву.
    if (isModifierKey(e.code, e.key)) {
      const mods = getModifiersFromEvent(e);
      setHotkeyRecording(document.getElementById(recordingField), mods);
      return;
    }

    const keyToken = codeToKeyToken(e.code, e.key);
    if (!keyToken) {
      // Пользователь нажал неподдерживаемую клавишу (Space, Tab, Enter, стрелки и т.п.)
      const keyName = e.key && e.key.length === 1 ? e.key : e.code || e.key || '?';
      setStatus(t('control.unsupportedKey', { key: keyName }));
      return;
    }

    const mods = getModifiersFromEvent(e);
    const res = buildHotkeyCombo(mods, keyToken);
    if (!res.success) {
      // Пользователь нажал клавишу без модификаторов — говорим об этом прямо
      if (res.error === 'modifier_required') {
        setStatus(t('control.modifierRequired'));
      }
      return;
    }

    const combo = res.combo;
    const [, key] = HOTKEY_FIELDS.find(([id]) => id === recordingField);
    if (recordingField === OPEN_GROUP_FIELD) {
      // Цифра из набранного сочетания не важна: пользователь задаёт
      // модификаторы, а цифру каждой группе присваиваем сами.
      const spread = spreadDigits(combo);
      if (!spread) return;
      draftHotkeys[key] = spread;
    } else {
      draftHotkeys[key] = combo;
    }
    const input = document.getElementById(recordingField);
    if (input) {
      // Для поля открытия группы это не набранная комбинация (в ней цифра
      // случайная — в значение она и так не попадает, см. spreadDigits), а
      // та же форма, что в renderControl: модификаторы + подставной хвост.
      setHotkeyDisplay(input, hotkeyFieldValue(key, draftHotkeys[key]));
      input.classList.remove('recording');
    }
    recordingField = null;
    setStatus('');
  },
  true
);

window.addEventListener(
  'keyup',
  (e) => {
    if (!recordingField) return;
    e.preventDefault();
    e.stopPropagation();
    const mods = getModifiersFromEvent(e);
    setHotkeyRecording(document.getElementById(recordingField), mods);
  },
  true
);

// Отмена записи при клике мимо полей хоткеев или потере фокуса окном
document.addEventListener('click', (e) => {
  if (recordingField && !e.target.closest('.hotkeyField')) {
    cancelRecording();
  }
});
window.addEventListener('blur', () => {
  cancelRecording();
});

// Кнопки «×» рядом с полями хоткеев — по одной на каждое поле, кроме
// режима редактирования: без него в программу нельзя войти вообще, и
// стирать его нечем по замыслу.
//
// Раньше обработчики были выписаны поштучно, и три хоткея групп, добавленные
// позже, остались с кнопками, которые ничего не делали (найдено при ревью
// 2026-08-26). Список полей уже есть — берём его, чтобы новое поле не могло
// снова остаться без обработчика.
for (const [elId, key] of HOTKEY_FIELDS) {
  if (elId === 'hotkey-edit') continue;
  const button = document.getElementById(`clear-${elId}`);
  if (!button) continue;
  button.addEventListener('click', () => {
    cancelRecording();
    // Поле открытия группы хранит массив из девяти строк: очистка — пустой
    // массив, а не null, иначе Rust прочитал бы отсутствие поля как «взять
    // значение по умолчанию» и хоткей вернулся бы сам собой.
    draftHotkeys[key] = key === 'open_group_by_number' ? [] : null;
    setHotkeyDisplay(document.getElementById(elId), '');
  });
}

// ==== Вкладка «Стикеры» ====

// Держать в синхроне с rst_core::model::VIDEO_EXTENSIONS.
const VIDEO_EXTENSIONS = [
  'mp4', 'm4v', 'webm', 'mkv', 'mov', 'avi', 'wmv', 'flv',
  'mpg', 'mpeg', 'ts', 'm2ts', 'mts', '3gp', 'ogv',
];
const IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'webp', 'bmp', 'gif'];

function mediaTypeLabel(source) {
  // StickerSource — internally tagged (#[serde(tag = "kind")]), значения
  // snake_case: "file" | "window" | "pasted".
  if (source?.kind === 'window') return t('media.window');
  const mt = source?.media_type;
  if (mt === 'video') return t('media.video');
  if (mt === 'animation') return t('media.animation');
  return t('media.image');
}

/** Подпись монитора для строки списка. Сырой monitor_id — device interface
 *  path на девяносто знаков (ADR-010), одинаковый у всех строк: он занимал
 *  всю мета-строку и не сообщал ничего. Берём дружественное имя из
 *  config.monitors, а если записи нет — модель из самого пути. */
function monitorLabel(monitorId) {
  const rec = (config.monitors ?? []).find((m) => m.id === monitorId);
  const name = rec?.friendly_name?.trim();
  if (name) return name;
  const m = /DISPLAY#([^#]+)#/.exec(monitorId ?? '');
  return m ? m[1] : monitorId || '?';
}

function stickerFilePath(sticker) {
  const source = sticker.source;
  if (source?.kind === 'file' || source?.kind === 'pasted') return source.path;
  return null;
}

function renderStickers() {
  const list = document.getElementById('sticker-list');
  list.innerHTML = '';
  const stickers = config.stickers ?? [];
  if (stickers.length === 0) {
    list.innerHTML = `<p class="emptyHint">${t('stickers.empty')}</p>`;
    return;
  }
  // На одном мониторе подпись монитора одинакова у всех строк — это не
  // сведения, а шум во всю ширину. Показываем её, только когда есть из чего
  // выбирать.
  const monitorsUsed = new Set(stickers.map((s) => s.placement?.monitor_id));
  const showMonitor = monitorsUsed.size > 1 || (config.monitors ?? []).length > 1;

  for (const sticker of stickers) {
    const row = document.createElement('div');
    row.className = 'stickerRow' + (sticker.visible ? '' : ' disabled');

    const path = stickerFilePath(sticker);
    const name = path ? path.split(/[\\/]/).pop() : `(${t('media.window').toLowerCase()})`;

    row.innerHTML = `
      <label class="checkboxWrapper" title="${t('sticker.toggleTitle')}" style="flex:none">
        <input type="checkbox" data-action="toggle" data-id="${sticker.id}" ${sticker.visible ? 'checked' : ''} />
        <span class="checkmark"></span>
      </label>
      <div class="stickerInfo">
        <span class="stickerName">${escapeHtml(name)}</span>
        <span class="stickerMeta">${mediaTypeLabel(sticker.source)}${
          showMonitor
            ? ` · ${t('sticker.monitorLabel', { id: escapeHtml(monitorLabel(sticker.placement?.monitor_id)) })}`
            : ''
        }</span>
      </div>
      <div class="stickerActions">
        <button class="iconBtn" data-action="reset-pos" data-id="${sticker.id}" title="${t('sticker.resetPosTitle')}" aria-label="${t('sticker.resetPosTitle')}">${icon('recenter')}</button>
        <button class="iconBtn" data-action="reset-transform" data-id="${sticker.id}" title="${t('sticker.resetTransformTitle')}" aria-label="${t('sticker.resetTransformTitle')}">${icon('restore')}</button>
        <button class="iconBtn" data-action="reveal" data-id="${sticker.id}" title="${t('sticker.revealTitle')}" aria-label="${t('sticker.revealTitle')}" ${path ? '' : 'disabled'}>${icon('folder')}</button>
        <button class="iconBtn" data-action="relink" data-id="${sticker.id}" title="${t('sticker.relinkTitle')}" aria-label="${t('sticker.relinkTitle')}" ${path ? '' : 'disabled'}>${icon('link')}</button>
        <button class="iconBtn danger" data-action="delete" data-id="${sticker.id}" title="${t('sticker.deleteTitle')}" aria-label="${t('sticker.deleteTitle')}">${icon('trash')}</button>
      </div>
    `;
    list.appendChild(row);
  }
}

function escapeHtml(s) {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}

document.getElementById('sticker-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('[data-action]');
  if (!btn) return;
  const id = btn.dataset.id;
  const action = btn.dataset.action;
  try {
    switch (action) {
      case 'reset-pos':
        await invoke('reset_sticker_position', { id });
        break;
      case 'reset-transform':
        await invoke('reset_sticker_transform', { id });
        break;
      case 'delete':
        await invoke('delete_sticker', { id });
        break;
      case 'reveal': {
        const sticker = config.stickers.find((s) => s.id === id);
        const path = stickerFilePath(sticker);
        if (path) await invoke('reveal_in_explorer', { path });
        break;
      }
      case 'relink': {
        const path = await open({
          multiple: false,
          filters: [
            { name: t('stickers.dialogFilter'), extensions: [...IMAGE_EXTENSIONS, ...VIDEO_EXTENSIONS] },
          ],
        });
        if (path) await invoke('relink_sticker', { id, path });
        break;
      }
      default:
        return;
    }
    await loadConfig();
  } catch (err) {
    stickerStatusEl.textContent = t('error.generic', { err });
  }
});

document.getElementById('sticker-list').addEventListener('change', async (e) => {
  const input = e.target.closest('[data-action="toggle"]');
  if (!input) return;
  try {
    await invoke('set_sticker_enabled', { id: input.dataset.id, enabled: input.checked });
    await loadConfig();
  } catch (err) {
    stickerStatusEl.textContent = t('error.generic', { err });
  }
});

document.getElementById('add-sticker').addEventListener('click', async () => {
  try {
    const path = await open({
      multiple: false,
      filters: [
        { name: t('stickers.dialogFilter'), extensions: [...IMAGE_EXTENSIONS, ...VIDEO_EXTENSIONS] },
      ],
    });
    if (path) {
      await invoke('add_sticker', { path });
      stickerStatusEl.textContent = t('stickers.added', { path });
      await loadConfig();
    }
  } catch (err) {
    stickerStatusEl.textContent = t('error.generic', { err });
  }
});

document.getElementById('reset-all-stickers').addEventListener('click', async () => {
  await invoke('reset_all_stickers');
  await loadConfig();
  setStatus(t('stickers.resetAllDone'));
});

document.getElementById('delete-all-stickers').addEventListener('click', async () => {
  if (!confirm(t('stickers.deleteAllConfirm'))) return;
  await invoke('delete_all_stickers');
  await loadConfig();
  setStatus(t('stickers.deleteAllDone'));
});

// ==== Вкладка «Пресеты» (M7) ====

// Модель Preset — { id, name, stickers } (crates/rst-core/src/model.rs):
// даты в ней нет, поэтому строка списка показывает число стикеров; если
// бэкенд добавит `created_at`, показываем его (как мета-строку вкладки
// «Стикеры»).
function presetMeta(preset) {
  if (preset.created_at) {
    return new Date(preset.created_at).toLocaleDateString('en-US');
  }
  const n = (preset.stickers ?? []).length;
  return t('preset.stickersCount', { n, word: pluralStickerWord(n) });
}

function renderPresets() {
  const list = document.getElementById('preset-list');
  list.innerHTML = '';
  const presets = config.presets ?? [];
  if (presets.length === 0) {
    list.innerHTML = `<p class="emptyHint">${t('presets.empty')}</p>`;
    return;
  }
  for (const preset of presets) {
    const row = document.createElement('div');
    row.className = 'stickerRow';
    row.innerHTML = `
      <div class="stickerInfo">
        <span class="stickerName">${escapeHtml(preset.name)}</span>
        <span class="stickerMeta">${escapeHtml(presetMeta(preset))}</span>
      </div>
      <div class="stickerActions">
        <button class="button compact" data-action="apply" data-id="${preset.id}" title="${t('preset.applyTitle')}">${t('preset.applyAction')}</button>
        <button class="iconBtn" data-action="rename" data-id="${preset.id}" title="${t('preset.renameTitle')}" aria-label="${t('preset.renameTitle')}">${icon('pencil')}</button>
        <button class="iconBtn" data-action="export" data-id="${preset.id}" title="${t('preset.exportTitle')}" aria-label="${t('preset.exportAction')}">${icon('export')}</button>
        <button class="iconBtn danger" data-action="delete" data-id="${preset.id}" title="${t('preset.deleteTitle')}" aria-label="${t('preset.deleteTitle')}">${icon('trash')}</button>
      </div>
    `;
    list.appendChild(row);
  }
}

// Вернуться из режима переименования в «сохранить как пресет».
function resetRenameMode() {
  renameTargetId = null;
  document.getElementById('save-preset').textContent = t('presets.save');
  presetStatusEl.textContent = '';
}

document.getElementById('preset-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('[data-action]');
  if (!btn) return;
  const id = btn.dataset.id;
  const action = btn.dataset.action;
  const preset = (config.presets ?? []).find((p) => p.id === id);
  try {
    switch (action) {
      case 'apply':
        // Список недоступных элементов приходит отдельным событием
        // 'preset-missing-elements' (обработчик ниже) — диалог недостающих
        // элементов, SPEC §11.
        await invoke('apply_preset', { id });
        setStatus(t('preset.applied', { name: preset?.name ?? id }));
        break;
      case 'rename': {
        renameTargetId = id;
        document.getElementById('preset-name').value = preset?.name ?? '';
        document.getElementById('save-preset').textContent = t('presets.rename');
        document.getElementById('save-preset').disabled = false;
        presetStatusEl.textContent = t('preset.renamingStatus', { name: preset?.name ?? '' });
        return; // список не перезагружаем, строка не удаляется
      }
      case 'export': {
        // SPEC §11 / CONFIG.md: внутри файла абсолютные пути к файлам
        // пользователя и имена программ — предупреждение обязательно.
        if (!confirm(t('preset.exportConfirm'))) {
          return;
        }
        const path = await save({
          defaultPath: `${String(preset?.name ?? 'preset').replace(/[\\/:*?"<>|]/g, '_')}.json`,
          filters: [{ name: t('presets.dialogFilter'), extensions: ['json'] }],
        });
        if (!path) return; // отмена в диалоге сохранения
        await invoke('export_preset', { id, path });
        setStatus(t('preset.exported', { path }));
        break;
      }
      case 'delete':
        if (!confirm(t('preset.deleteConfirm', { name: preset?.name ?? '' }))) return;
        await invoke('delete_preset', { id });
        setStatus(t('preset.deleted'));
        break;
      default:
        return;
    }
    await loadConfig();
  } catch (err) {
    presetStatusEl.textContent = t('error.generic', { err });
  }
});

const presetNameInput = document.getElementById('preset-name');
const savePresetBtn = document.getElementById('save-preset');

presetNameInput.addEventListener('input', () => {
  savePresetBtn.disabled = !presetNameInput.value.trim();
});
presetNameInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !savePresetBtn.disabled) savePresetBtn.click();
});

savePresetBtn.addEventListener('click', async () => {
  const name = presetNameInput.value.trim();
  if (!name) return;
  try {
    if (renameTargetId) {
      await invoke('rename_preset', { id: renameTargetId, name });
      setStatus(t('preset.renamed'));
    } else {
      await invoke('save_preset', { name });
      setStatus(t('preset.saved', { name }));
    }
    presetNameInput.value = '';
    resetRenameMode();
    savePresetBtn.disabled = true;
    await loadConfig();
  } catch (err) {
    presetStatusEl.textContent = t('error.generic', { err });
  }
});

document.getElementById('import-preset').addEventListener('click', async () => {
  try {
    const path = await open({
      multiple: false,
      filters: [{ name: t('presets.dialogFilter'), extensions: ['json'] }],
    });
    if (!path) return; // отмена в диалоге открытия
    await invoke('import_preset', { path });
    setStatus(t('preset.imported', { path }));
    await loadConfig();
  } catch (err) {
    presetStatusEl.textContent = t('error.generic', { err });
  }
});

// Окно не пересоздаётся между показами — оно просто прячется. Бэкенд шлёт
// это событие на каждый показ, чтобы список стикеров и пресетов был живым, а
// не таким, каким был при запуске программы.
listen('settings-shown', () => {
  loadConfig();
});

// Диалог недостающих элементов (SPEC §11): бэкенд применяет пресет без
// недоступных стикеров и шлёт их список событием 'preset-missing-elements'.
listen('preset-missing-elements', (event) => {
  const missing = event.payload?.missing ?? [];
  if (missing.length === 0) return;
  const lines = missing.map((m) => `• ${m.path ?? m[1]}`).join('\n');
  alert(t('preset.missingAlert', { lines }));
});

// ==== Вкладка «Денй-лист» (закрепление окон) ====

// Правила — OverlapRule { process_name, title_pattern } из
// cfg.settings.denylist (crates/rst-core/src/model.rs). Id у правила нет,
// поэтому строки адресуются индексом списка; канал команд FIFO, индексы UI
// и координатора не расходятся. Список рендерится из draftSettings, а не из
// config: сразу после add/remove диск может отставать от координатора на
// гонку invoke→get_config, черновик же правим мы сами (см. ниже).
function sameDenylistRule(a, b) {
  return (a?.process_name ?? null) === (b?.process_name ?? null)
    && (a?.title_pattern ?? null) === (b?.title_pattern ?? null);
}

function renderDenylist() {
  const list = document.getElementById('denylist-list');
  list.innerHTML = '';
  const rules = draftSettings.denylist ?? [];
  if (rules.length === 0) {
    list.innerHTML = `<p class="emptyHint">${t('denylist.empty')}</p>`;
    return;
  }
  for (let i = 0; i < rules.length; i++) {
    const rule = rules[i];
    const row = document.createElement('div');
    row.className = 'stickerRow';
    row.innerHTML = `
      <div class="stickerInfo">
        <span class="stickerName">${escapeHtml(rule.process_name ?? rule.title_pattern ?? '')}</span>
        <span class="stickerMeta">${escapeHtml(
          rule.process_name
            ? (rule.title_pattern ?? t('denylist.noTitlePattern'))
            : t('denylist.anyProcess')
        )}</span>
      </div>
      <div class="stickerActions">
        <button class="iconBtn danger" data-action="remove" data-index="${i}" title="${t('denylist.removeTitle')}" aria-label="${t('denylist.removeTitle')}">${icon('trash')}</button>
      </div>
    `;
    list.appendChild(row);
  }
}

const denylistProcessInput = document.getElementById('denylist-process');
const denylistTitleInput = document.getElementById('denylist-title');
const addDenylistBtn = document.getElementById('add-denylist-rule');
const denylistProcessPicker = document.getElementById('denylist-process-picker');

// Пикер процессов из открытых окон (запрос пользователя 2026-08-19: раньше
// process_name.exe приходилось печатать руками) — тот же принцип, что
// нативный window_pick_list.rs у оверлея, но списком в <select>: эта панель
// живёт в Tauri-вебвью, а не в D3D-рендере, отдельный набор виджетов не
// нужен, обычный select — родной контрол ОС, ничего лишнего верстать.
// Текстовое поле остаётся рабочим и без пикера — ручной ввод не убран,
// пикер только избавляет от необходимости печатать точное имя.
async function refreshDenylistProcessPicker() {
  let processes = [];
  try {
    processes = await invoke('list_open_processes');
  } catch {
    return; // тихо — пикер необязателен, ручной ввод всё ещё работает
  }
  const previous = denylistProcessPicker.value;
  denylistProcessPicker.innerHTML = `<option value="">${t('denylist.pickWindow')}</option>`;
  for (const [processName, title] of processes) {
    const opt = document.createElement('option');
    opt.value = processName;
    opt.textContent = `${title} — ${processName}`;
    denylistProcessPicker.appendChild(opt);
  }
  // Сохранить выбор, если тот же процесс всё ещё в списке (обновление по
  // смене вкладки не должно сбрасывать то, что пользователь уже выбрал).
  if (processes.some(([p]) => p === previous)) denylistProcessPicker.value = previous;
}

denylistProcessPicker.addEventListener('change', () => {
  if (!denylistProcessPicker.value) return;
  denylistProcessInput.value = denylistProcessPicker.value;
  updateAddDenylistBtn();
  denylistProcessInput.focus();
});

function updateAddDenylistBtn() {
  addDenylistBtn.disabled = !denylistProcessInput.value.trim();
}

denylistProcessInput.addEventListener('input', updateAddDenylistBtn);
for (const input of [denylistProcessInput, denylistTitleInput]) {
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !addDenylistBtn.disabled) addDenylistBtn.click();
  });
}

addDenylistBtn.addEventListener('click', async () => {
  const processName = denylistProcessInput.value.trim();
  const titlePattern = denylistTitleInput.value.trim() || null;
  if (!processName) return;
  const rule = { process_name: processName, title_pattern: titlePattern };
  try {
    await invoke('add_denylist_rule', { processName, titlePattern });
    await loadConfig();
    // Гонка invoke→get_config: свежепрочитанный config мог ещё не содержать
    // только что сохранённое правило (координатор пишет файл асинхронно).
    // Достраиваем черновик сами — иначе Apply/OK отправит update_settings
    // со старым списком и молча откатит добавление.
    if (!(config.settings.denylist ?? []).some((r) => sameDenylistRule(r, rule))) {
      draftSettings.denylist = [...(draftSettings.denylist ?? []), rule];
      renderDenylist();
    }
    setStatus(t('denylist.added', { process: processName }));
    denylistStatusEl.textContent = '';
    denylistProcessInput.value = '';
    denylistTitleInput.value = '';
    updateAddDenylistBtn();
  } catch (err) {
    denylistStatusEl.textContent = t('error.generic', { err });
  }
});

document.getElementById('denylist-list').addEventListener('click', async (e) => {
  const btn = e.target.closest('[data-action="remove"]');
  if (!btn) return;
  const index = Number(btn.dataset.index);
  const removed = (draftSettings.denylist ?? [])[index];
  if (!removed) return;
  try {
    await invoke('remove_denylist_rule', { index });
    await loadConfig();
    // Та же гонка с диском, что при добавлении: если свежепрочитанный
    // config всё ещё содержит удалённое правило, убираем его из черновика
    // сами — иначе Apply/OK вернёт его через update_settings.
    if ((config.settings.denylist ?? []).some((r) => sameDenylistRule(r, removed))) {
      const i = (draftSettings.denylist ?? []).findIndex((r) => sameDenylistRule(r, removed));
      if (i >= 0) {
        draftSettings.denylist.splice(i, 1);
        renderDenylist();
      }
    }
    setStatus(t('denylist.removed'));
  } catch (err) {
    denylistStatusEl.textContent = t('error.generic', { err });
  }
});

// ==== Подвал: Применить / ОК / Отмена ====

const applyButton = document.getElementById('apply');
const okButton = document.getElementById('ok');
let applying = false;

async function applyChanges() {
  // Обе кнопки запускают один двухшаговый commit: повторный клик в середине
  // мог отправить вторую пару команд и оставить настройки в смешанном виде.
  if (applying) return false;
  applying = true;
  applyButton.disabled = true;
  okButton.disabled = true;
  try {
    await invoke('update_settings', { settings: draftSettings });
    await invoke('update_hotkeys', { hotkeys: draftHotkeys });
    setStatus(t('footer.applied'));
    return true;
  } catch (err) {
    setStatus(t('error.generic', { err }));
    return false;
  } finally {
    applying = false;
    applyButton.disabled = false;
    okButton.disabled = false;
  }
}

applyButton.addEventListener('click', () => {
  void applyChanges();
});

okButton.addEventListener('click', async () => {
  if (await applyChanges()) {
    await getCurrentWindow().close();
  }
});

document.getElementById('cancel').addEventListener('click', async () => {
  await loadConfig(); // отбросить черновик
  await getCurrentWindow().close();
});

document.getElementById('close').addEventListener('click', async () => {
  await getCurrentWindow().close();
});

loadConfig().then(() => {
  // Свет ленты ставится без анимации: при открытии окна он должен уже
  // лежать под активной вкладкой, а не приезжать под неё из угла.
  moveTabLight(false);
  updateScrollEdges();
});
