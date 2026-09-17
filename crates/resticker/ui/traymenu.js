// Меню иконки в трее — своё окно вместо всплывающего меню Windows.
//
// Почему не системное меню: его рисует сама Windows, и owner-draw дотягивался
// только до плашек пунктов — ни прозрачности, ни размытия подложки, ни
// настоящих скруглений у него нет, а градиент GDI кладёт полосами. Пятьсот
// строк такой отрисовки давали «почти похоже» и всё равно выбивались из
// программы (репорт пользователя 2026-09-17). Здесь тот же CSS и те же токены,
// что у окна настроек, поэтому расходиться им больше не с чем.
//
// Без фреймворка и без бандлера — тот же принцип, что у main.js.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const menuEl = document.getElementById('menu');
const presetsEl = document.getElementById('presets');
const presetsBlock = document.getElementById('presets-block');
const snapGapValueEl = document.getElementById('snap-gap-value');
const snapGapAllTick = document.getElementById('snap-gap-all-tick');

/** Пресеты текущего конфига: индекс строки → id, как в списке настроек. */
let presets = [];
/** Пока идёт действие, второй клик игнорируется: окно всё равно закрывается. */
let busy = false;

/** Перевести статические подписи (`data-i18n`) — те же ключи, что в настройках. */
function applyLabels() {
  for (const el of document.querySelectorAll('[data-i18n]')) {
    el.textContent = t(el.getAttribute('data-i18n'));
  }
}

/** Перечитать состояние с бэкенда: пункты меню обязаны показывать текущее, а
 *  не то, что было при первом открытии (окно живёт между показами). */
async function refresh() {
  let config = null;
  try {
    config = await invoke('get_config');
  } catch (err) {
    // Меню без состояния всё равно полезно: настройки и выход работают.
    console.warn('tray menu: config unavailable', err);
  }
  const pct = config?.settings?.snap_shrink_pct ?? 0;
  snapGapValueEl.textContent = pct > 0 ? `${pct}%` : '';
  snapGapAllTick.classList.toggle('off', !config?.settings?.snap_shrink_all_windows);

  presets = config?.presets ?? [];
  presetsBlock.hidden = presets.length === 0;
  presetsEl.replaceChildren(
    ...presets.map((preset, i) => {
      const btn = document.createElement('button');
      btn.className = 'item';
      btn.type = 'button';
      btn.dataset.action = 'preset';
      btn.dataset.index = String(i);
      // Пустое место под иконку: без него подписи пресетов вставали левее
      // остальных пунктов, и колонка текста ломалась посреди меню.
      const spacer = document.createElement('span');
      spacer.className = 'tick off';
      spacer.setAttribute('aria-hidden', 'true');
      btn.append(spacer);
      const label = document.createElement('span');
      label.className = 'label';
      label.textContent = preset.name;
      btn.append(label);
      return btn;
    }),
  );
}

/** Сообщить бэкенду итоговый размер, чтобы он поставил окно у курсора и
 *  показал его. Размер считается ПОСЛЕ отрисовки: число пресетов меняет
 *  высоту, а пустое окно у курсора выглядело бы дырой в экране. */
async function reportSize() {
  const rect = menuEl.getBoundingClientRect();
  await invoke('tray_menu_ready', {
    width: Math.ceil(rect.width),
    height: Math.ceil(rect.height),
  });
}

async function run(action) {
  if (busy) return;
  busy = true;
  try {
    await action();
  } catch (err) {
    console.warn('tray menu: action failed', err);
  } finally {
    busy = false;
    await invoke('hide_tray_menu');
  }
}

menuEl.addEventListener('click', (e) => {
  const btn = e.target.closest('.item');
  if (!btn) return;
  switch (btn.dataset.action) {
    case 'settings':
      void run(() => invoke('open_settings_window'));
      break;
    case 'edit-mode':
      void run(() => invoke('tray_toggle_edit_mode'));
      break;
    case 'toggle-visible':
      void run(() => invoke('tray_toggle_visible'));
      break;
    case 'snap-gap':
      void run(() => invoke('tray_open_gap_panel'));
      break;
    case 'snap-gap-all':
      void run(() => invoke('tray_toggle_snap_gap_all'));
      break;
    case 'preset': {
      const preset = presets[Number(btn.dataset.index)];
      if (preset) void run(() => invoke('apply_preset', { id: preset.id }));
      break;
    }
    case 'exit':
      // Выход закрывает процесс — прятать окно после него нечего и некому.
      busy = true;
      void invoke('tray_exit');
      break;
    default:
      break;
  }
});

// Esc закрывает меню, как и у любого всплывающего списка программы.
window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') void invoke('hide_tray_menu');
});

// Закрытие по уходу фокуса живёт в Rust (`on_window_event`): до страницы это
// событие не доходило — меню оставалось висеть.

// Каждый показ — свежее состояние: окно не пересоздаётся, поэтому опросить
// бэкенд один раз при загрузке недостаточно.
void listen('tray-menu-shown', () => {
  void refresh().then(reportSize);
});

applyLabels();
void refresh().then(reportSize);
