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

function setStatus(text) {
  statusEl.textContent = text;
  if (text) {
    setTimeout(() => {
      if (statusEl.textContent === text) statusEl.textContent = '';
    }, 4000);
  }
}

// ==== Вкладки ====

document.getElementById('tabs').addEventListener('click', (e) => {
  const tabEl = e.target.closest('.tab');
  const tab = tabEl?.dataset.tab;
  if (!tab) return;
  for (const b of document.querySelectorAll('.tab')) {
    b.classList.toggle('active', b.dataset.tab === tab);
  }
  for (const s of document.querySelectorAll('.panel')) {
    s.classList.toggle('active', s.dataset.panel === tab);
  }
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
  applyStaticTranslations(draftSettings.language ?? 'ru');
  renderGeneral();
  renderControl();
  renderStickers();
  renderPresets();
  renderDenylist();
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
  const fps = draftSettings.battery_fps_limit ?? 30;
  document.getElementById('battery-fps-limit').value = fps;
  document.getElementById('battery-fps-limit-value').textContent = String(fps);
  document.getElementById('language').value = draftSettings.language ?? 'ru';
}

const GENERAL_CHECKBOXES = [
  ['autostart', 'autostart'],
  ['silent-start', 'silent_start'],
  ['tray-icon', 'tray_icon'],
  ['skip-delete-confirmation', 'skip_delete_confirmation'],
  ['hide-from-capture', 'hide_from_capture'],
  ['never-overlap-taskbar', 'never_overlap_taskbar'],
];

for (const [elId, key] of GENERAL_CHECKBOXES) {
  document.getElementById(elId).addEventListener('change', (e) => {
    draftSettings[key] = e.target.checked;
  });
}

const fpsSlider = document.getElementById('battery-fps-limit');
const fpsValue = document.getElementById('battery-fps-limit-value');
fpsSlider.addEventListener('input', () => {
  fpsValue.textContent = fpsSlider.value;
  draftSettings.battery_fps_limit = Number(fpsSlider.value);
});

// Смена языка применяется сразу, без перезапуска окна (перерисовываем и
// статический, и динамический текст — вкладки «Стикеры»/«Пресеты» строят
// разметку через t() на каждый рендер) — сохраняется в draftSettings и
// уходит в конфиг обычным путём, вместе с остальными настройками, по
// «Применить»/«ОК».
document.getElementById('language').addEventListener('change', (e) => {
  draftSettings.language = e.target.value;
  applyStaticTranslations(draftSettings.language);
  renderStickers();
  renderPresets();
  renderDenylist();
});

// ==== Вкладка «Управление» (хоткеи) ====

const HOTKEY_FIELDS = [
  ['hotkey-edit', 'edit_mode'],
  ['hotkey-toggle-all', 'toggle_all_stickers'],
  ['hotkey-mute-all', 'mute_all'],
];

function renderControl() {
  for (const [elId, key] of HOTKEY_FIELDS) {
    document.getElementById(elId).value = draftHotkeys[key] ?? '';
  }
}

function codeToKeyToken(code) {
  if (code.startsWith('Key') && code.length === 4) return code.slice(3);
  if (code.startsWith('Digit') && code.length === 6) return code.slice(5);
  if (/^F([1-9]|1\d|2[0-4])$/.test(code)) return code;
  return null;
}

// Отменить запись текущего поля: вернуть его значение из draftHotkeys (а не
// оставить плейсхолдер "Нажмите комбинацию…" висеть) и снять .recording.
// Общий путь для Esc И для клика по ДРУГОМУ полю хоткея, пока это ещё
// записывает — раньше клик по другому полю чистил только CSS-класс, само
// значение оставалось залипшим на плейсхолдере (найдено независимым ревью).
function cancelRecording() {
  if (!recordingField) return;
  const [, key] = HOTKEY_FIELDS.find(([id]) => id === recordingField);
  document.getElementById(recordingField).value = draftHotkeys[key] ?? '';
  document.getElementById(recordingField).classList.remove('recording');
  recordingField = null;
}

for (const [elId] of HOTKEY_FIELDS) {
  const input = document.getElementById(elId);
  input.addEventListener('click', () => {
    cancelRecording();
    recordingField = elId;
    input.classList.add('recording');
    input.value = t('control.recording');
  });
}

document.addEventListener('keydown', (e) => {
  if (!recordingField) return;
  e.preventDefault();
  if (e.code === 'Escape') {
    cancelRecording();
    return;
  }
  const keyToken = codeToKeyToken(e.code);
  if (!keyToken) return; // ждём буквенно-цифровую клавишу или F1-F24
  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Win');
  if (parts.length === 0) return; // HotkeyCombo::parse требует хотя бы один модификатор
  parts.push(keyToken);
  const combo = parts.join('+');

  const [, key] = HOTKEY_FIELDS.find(([id]) => id === recordingField);
  draftHotkeys[key] = combo;
  document.getElementById(recordingField).value = combo;
  document.getElementById(recordingField).classList.remove('recording');
  recordingField = null;
});

document.getElementById('clear-hotkey-toggle-all').addEventListener('click', () => {
  draftHotkeys.toggle_all_stickers = null;
  document.getElementById('hotkey-toggle-all').value = '';
});
document.getElementById('clear-hotkey-mute-all').addEventListener('click', () => {
  draftHotkeys.mute_all = null;
  document.getElementById('hotkey-mute-all').value = '';
});

// ==== Вкладка «Стикеры» ====

const VIDEO_EXTENSIONS = ['mp4', 'webm', 'mkv', 'mov', 'avi'];
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
        <span class="stickerMeta">${mediaTypeLabel(sticker.source)} · ${t('sticker.monitorLabel', { id: escapeHtml(sticker.placement?.monitor_id ?? '?') })}</span>
      </div>
      <div class="stickerActions">
        <button class="button compact" data-action="reset-pos" data-id="${sticker.id}" title="${t('sticker.resetPosTitle')}">⤾</button>
        <button class="button compact" data-action="reset-transform" data-id="${sticker.id}" title="${t('sticker.resetTransformTitle')}">⟲</button>
        <button class="button compact" data-action="reveal" data-id="${sticker.id}" title="${t('sticker.revealTitle')}" ${path ? '' : 'disabled'}>📁</button>
        <button class="button compact" data-action="relink" data-id="${sticker.id}" title="${t('sticker.relinkTitle')}" ${path ? '' : 'disabled'}>↻</button>
        <button class="button compact danger" data-action="delete" data-id="${sticker.id}" title="${t('sticker.deleteTitle')}">✕</button>
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
    return new Date(preset.created_at).toLocaleDateString(currentLang === 'ru' ? 'ru-RU' : 'en-US');
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
        <button class="button compact" data-action="rename" data-id="${preset.id}" title="${t('preset.renameTitle')}">${t('presets.rename')}</button>
        <button class="button compact" data-action="export" data-id="${preset.id}" title="${t('preset.exportTitle')}">${t('preset.exportAction')}</button>
        <button class="button compact danger" data-action="delete" data-id="${preset.id}" title="${t('preset.deleteTitle')}">✕</button>
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
        <span class="stickerName">${escapeHtml(rule.process_name ?? '')}</span>
        <span class="stickerMeta">${escapeHtml(rule.title_pattern ?? t('denylist.noTitlePattern'))}</span>
      </div>
      <div class="stickerActions">
        <button class="button compact danger" data-action="remove" data-index="${i}" title="${t('denylist.removeTitle')}">✕</button>
      </div>
    `;
    list.appendChild(row);
  }
}

const denylistProcessInput = document.getElementById('denylist-process');
const denylistTitleInput = document.getElementById('denylist-title');
const addDenylistBtn = document.getElementById('add-denylist-rule');

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

async function applyChanges() {
  await invoke('update_settings', { settings: draftSettings });
  await invoke('update_hotkeys', { hotkeys: draftHotkeys });
  setStatus(t('footer.applied'));
}

document.getElementById('apply').addEventListener('click', () => {
  applyChanges();
});

document.getElementById('ok').addEventListener('click', async () => {
  await applyChanges();
  await getCurrentWindow().close();
});

document.getElementById('cancel').addEventListener('click', async () => {
  await loadConfig(); // отбросить черновик
  await getCurrentWindow().close();
});

document.getElementById('close').addEventListener('click', async () => {
  await getCurrentWindow().close();
});

loadConfig();
